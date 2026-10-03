//! Differential checking of Nix source against `nix-instantiate`, and the
//! record of the divergences found.
//!
//! [`check`] decides one input in three steps, both evaluators in pure mode:
//!
//! 1. Parse: whether ogygia-nix-eval compiles it must match whether
//!    `nix-instantiate --parse` accepts it, undefined variables included.
//! 2. Evaluate: the deeply evaluated values must be equal, or both sides
//!    must fail.
//! 3. When both fail, `builtins.tryEval` must catch the failure on both
//!    sides or on neither. Error messages are not compared.
//!
//! Inputs where Nix runs out of time, memory or stack say nothing about
//! equivalence and are skipped before ogygia-nix-eval sees them.
//!
//! A findings directory holds one subdirectory per divergent input, named
//! by a hash of the input, with the input as `input.nix` and a
//! human-readable `report`.
//!
//! `nix-instantiate` is `$OGYGIA_NIX_EVAL_NIX_INSTANTIATE`, else the path
//! baked in at build time from `$OGYGIA_NIX_INSTANTIATE_BIN`, else found on
//! `PATH`.

use std::io::Read;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use ogygia_nix_eval::Context;
use ogygia_nix_eval::Io;
use ogygia_nix_eval::Settings;
use ogygia_nix_eval::run_with_stack;
use sha2::Digest;

const DEEP_COPY: &str = "let dc = v: if builtins.isAttrs v then builtins.mapAttrs (_: dc) v \
                         else if builtins.isList v then map dc v else v; in dc";

/// How long Nix may spend on one expression before the input is discarded.
const NIX_TIMEOUT: Duration = Duration::from_secs(10);

/// Address space Nix may use before the input is discarded. It is half of
/// libFuzzer's default RSS limit, so ogygia-nix-eval running out of memory
/// on an input means it needed several times what Nix did.
const NIX_MEMORY: libc::rlim_t = 1 << 30;

const DEAD_PROXY: &str = "http://127.0.0.1:9";

const PROXY_VARS: &[&str] = &[
    "http_proxy",
    "https_proxy",
    "ftp_proxy",
    "all_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "FTP_PROXY",
    "ALL_PROXY",
];

const NIX_ARGS: &[&str] = &[
    "--readonly-mode",
    "--store",
    "dummy://",
    "--option",
    "pure-eval",
    "true",
];

/// Point this process, and the Nix processes it starts, at a proxy that
/// refuses connections, so fetches fail without reaching the network.
///
/// # Safety
///
/// Must be called before any other thread exists.
pub unsafe fn block_network() {
    for var in PROXY_VARS {
        // SAFETY: the caller guarantees no other thread exists.
        unsafe { std::env::set_var(var, DEAD_PROXY) };
    }
    for var in ["no_proxy", "NO_PROXY"] {
        // SAFETY: as above.
        unsafe { std::env::remove_var(var) };
    }
}

/// What [`check`] made of an input.
pub enum Outcome {
    /// Both sides reject it.
    ParseRejected,
    /// Nix ran out of time, memory or stack.
    Skipped,
    /// Both sides evaluate it to the same value.
    Value,
    /// Both sides fail, and `builtins.tryEval` catches it on both.
    Caught,
    /// Both sides fail, and `builtins.tryEval` catches it on neither.
    Uncaught,
    /// The two disagree; holds a report of the step that diverged, the
    /// input, and both sides' full output.
    Diverged(String),
}

impl Outcome {
    pub const NAMES: [&str; 6] = [
        "parse-rejected",
        "skipped",
        "values",
        "caught",
        "uncaught",
        "diverged",
    ];

    /// Index into [`Outcome::NAMES`].
    pub fn index(&self) -> usize {
        match self {
            Outcome::ParseRejected => 0,
            Outcome::Skipped => 1,
            Outcome::Value => 2,
            Outcome::Caught => 3,
            Outcome::Uncaught => 4,
            Outcome::Diverged(_) => 5,
        }
    }
}

/// Standard output on success, standard error on failure.
type Run = Result<String, String>;

fn scratch() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("ogygia-nix-eval-fuzz");
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    })
}

fn nix_instantiate() -> String {
    std::env::var("OGYGIA_NIX_EVAL_NIX_INSTANTIATE")
        .ok()
        .or_else(|| option_env!("OGYGIA_NIX_INSTANTIATE_BIN").map(str::to_owned))
        .unwrap_or_else(|| "nix-instantiate".to_owned())
}

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        r.read_to_end(&mut buf)
            .expect("reading nix-instantiate output");
        buf
    })
}

/// `None` when Nix ran out of time, memory or stack, which says nothing
/// about equivalence.
fn run_nix(mode: &[&str], expr: &str) -> Option<Run> {
    let s = scratch();
    let mut cmd = Command::new(nix_instantiate());
    cmd.args(mode)
        .args(NIX_ARGS)
        // On standard input, the expression cannot be mistaken for a flag and
        // may contain NUL bytes.
        .arg("-")
        .current_dir("/")
        .env_clear()
        .env("HOME", s)
        .env("NIX_STATE_DIR", s.join("state"))
        .env("NIX_CONF_DIR", s.join("conf"))
        .env("NIX_LOG_DIR", s.join("log"))
        .env("XDG_CACHE_HOME", s.join("cache"))
        .envs(PROXY_VARS.iter().map(|v| (v, DEAD_PROXY)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let limit = libc::rlimit {
        rlim_cur: NIX_MEMORY,
        rlim_max: NIX_MEMORY,
    };
    // SAFETY: setrlimit is async-signal-safe and touches no memory of ours.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_AS, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let mut child = cmd.spawn().expect("running nix-instantiate");
    let stdout = read_all(child.stdout.take().unwrap());
    let stderr = read_all(child.stderr.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    let input = expr.as_bytes().to_vec();
    let stdin = std::thread::spawn(move || {
        // Nix stops reading early when it is killed.
        if let Err(e) = stdin.write_all(&input)
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            panic!("writing nix-instantiate input: {e}");
        }
    });
    let deadline = Instant::now() + NIX_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting for nix-instantiate") {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().expect("killing nix-instantiate");
            child.wait().expect("waiting for nix-instantiate");
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    stdin.join().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap()).into_owned();
    let status = status?;
    let exhausted = [
        "out of memory",
        "bad_alloc",
        "stack overflow",
        "max-call-depth",
    ];
    if exhausted.iter().any(|e| stderr.contains(e)) {
        return None;
    }
    Some(if status.success() {
        Ok(String::from_utf8_lossy(&stdout).trim_end().to_owned())
    } else {
        Err(stderr)
    })
}

fn pure() -> Settings {
    Settings {
        current_system: None,
        nix_path: Vec::new(),
        pure: true,
    }
}

fn show(r: &Run) -> String {
    match r {
        Ok(s) => s.clone(),
        Err(e) => format!("error: {}", e.trim_end()),
    }
}

fn diverged(step: &str, src: &str, expr: &str, theirs: &Run, ours: &Run) -> Outcome {
    Outcome::Diverged(format!(
        "step: {step}\n--- input\n{src}\n--- expression\n{expr}\n--- nix\n{}\n--- ours\n{}\n",
        show(theirs),
        show(ours)
    ))
}

/// Both sides succeed with the same output, or both fail.
fn agree(theirs: &Run, ours: &Run) -> bool {
    match (theirs, ours) {
        (Ok(a), Ok(b)) => a == b,
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

/// Compare ogygia-nix-eval with Nix on the expression `src`.
pub fn check(src: &str) -> Outcome {
    let ours: Run = run_with_stack(|| {
        Context::new(Io::default())
            .compile_str(src, "/")
            .map(|_| String::new())
            .map_err(|e| e.msg)
    });
    let Some(theirs) = run_nix(&["--parse"], src) else {
        return Outcome::Skipped;
    };
    if theirs.is_ok() != ours.is_ok() {
        return diverged("parse", src, src, &theirs, &ours);
    }
    if ours.is_err() {
        return Outcome::ParseRejected;
    }

    // The input parses on its own, so parenthesised on lines of its own it
    // is the same expression.
    let expr = format!("{DEEP_COPY} (\n{src}\n)");
    let Some(theirs) = run_nix(&["--eval", "--strict"], &expr) else {
        return Outcome::Skipped;
    };
    let ours = ogygia_nix_eval::eval_to_string(&expr, "/", pure());
    if !agree(&theirs, &ours) {
        return diverged("eval", src, &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        return Outcome::Value;
    }

    let expr = format!("builtins.tryEval (builtins.deepSeq (\n{src}\n) null)");
    let Some(theirs) = run_nix(&["--eval", "--strict"], &expr) else {
        return Outcome::Skipped;
    };
    let ours = ogygia_nix_eval::eval_to_string(&expr, "/", pure());
    if !agree(&theirs, &ours) {
        return diverged("catch", src, &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        Outcome::Caught
    } else {
        Outcome::Uncaught
    }
}

/// Replace `dir/name` with `contents` without readers seeing a partial file.
fn write_atomic(dir: &Path, name: &str, contents: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, dir.join(name))
}

/// Add `src`, on which the two evaluators disagree as `report` describes,
/// to the findings in `findings`. Safe to call from several processes at
/// once.
pub fn record(findings: &Path, src: &str, report: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(findings)?;
    let hash = sha2::Sha256::digest(src.as_bytes());
    let dir = findings.join(hex::encode(&hash[..16]));
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        // The same input was recorded before.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e),
    }
    write_atomic(&dir, "input.nix", src)?;
    write_atomic(&dir, "report", report)
}

/// What [`recheck`] did.
#[derive(Default)]
pub struct Recheck {
    /// Still diverging; their reports are refreshed.
    pub kept: usize,
    /// No longer diverging, so deleted.
    pub removed: usize,
    /// Nix ran out of time, memory or stack, so left as they were.
    pub inconclusive: usize,
}

/// Check every finding in `findings` again with this build, keeping only
/// those that still diverge. Must not run while anything records findings.
pub fn recheck(findings: &Path) -> std::io::Result<Recheck> {
    let mut summary = Recheck::default();
    let Ok(entries) = std::fs::read_dir(findings) else {
        return Ok(summary);
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') || !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = entry.path();
        let Ok(src) = std::fs::read_to_string(dir.join("input.nix")) else {
            std::fs::remove_dir_all(&dir)?;
            summary.removed += 1;
            continue;
        };
        match check(&src) {
            Outcome::Diverged(report) => {
                write_atomic(&dir, "report", &report)?;
                summary.kept += 1;
            }
            Outcome::Skipped => summary.inconclusive += 1,
            _ => {
                std::fs::remove_dir_all(&dir)?;
                summary.removed += 1;
            }
        }
    }
    Ok(summary)
}

/// Counts outcomes and reports them every 15 seconds, as a Unix time
/// followed by name and count pairs for the inputs since the last report:
/// appended to `file` when given, which is safe from several processes at
/// once, else to standard error.
pub fn count(outcome: &Outcome, file: Option<&Path>) {
    static PENDING: Mutex<Option<(Instant, [u64; 6])>> = Mutex::new(None);
    let mut pending = PENDING.lock().unwrap();
    let (since, counts) = pending.get_or_insert_with(|| (Instant::now(), [0; 6]));
    counts[outcome.index()] += 1;
    if since.elapsed() < Duration::from_secs(15) {
        return;
    }
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut line = time.to_string();
    for (name, n) in Outcome::NAMES.iter().zip(&*counts) {
        line += &format!(" {name} {n}");
    }
    line.push('\n');
    match file {
        // One write per line, so lines from concurrent processes stay whole.
        Some(path) => std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .expect("writing fuzzing statistics"),
        None => eprint!("outcomes: {line}"),
    }
    *pending = None;
}
