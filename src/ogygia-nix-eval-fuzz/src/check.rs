//! Comparing ogygia-nix-eval with `nix-instantiate` on one input.

use std::io::Read;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use ogygia_nix_eval::Context;
use ogygia_nix_eval::Io;
use ogygia_nix_eval::Settings;
use ogygia_nix_eval::run_with_stack;

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
    /// It contains a NUL byte, at which Nix stops reading its input.
    Ignored,
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
    pub const NAMES: [&str; 7] = [
        "ignored",
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
            Outcome::Ignored => 0,
            Outcome::ParseRejected => 1,
            Outcome::Skipped => 2,
            Outcome::Value => 3,
            Outcome::Caught => 4,
            Outcome::Uncaught => 5,
            Outcome::Diverged(_) => 6,
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

/// Compare ogygia-nix-eval with Nix on the expression `src`, both in pure
/// mode, in up to three steps:
///
/// 1. Parse: whether ogygia-nix-eval compiles it must match whether
///    `nix-instantiate --parse` accepts it, undefined variables included.
/// 2. Evaluate: the deeply evaluated values must be equal, or both sides
///    must fail.
/// 3. When both fail, `builtins.tryEval` must catch the failure on both
///    sides or on neither. Error messages are not compared.
///
/// Nix runs first at each step, and an input on which it runs out of time,
/// memory or stack is [skipped](Outcome::Skipped).
/// An input containing a NUL byte is [ignored](Outcome::Ignored): Nix
/// stops reading at the first one outside a string, so it evaluates a
/// prefix of the input rather than the input.
pub fn check(src: &str) -> Outcome {
    check_with(
        |expr| ogygia_nix_eval::eval_to_string(expr, "/", pure()),
        src,
    )
}

/// [`check`], with `eval` standing in for ogygia-nix-eval's evaluation.
fn check_with(eval: impl Fn(&str) -> Run, src: &str) -> Outcome {
    if src.contains('\0') {
        return Outcome::Ignored;
    }
    let ours: Run = run_with_stack(|| {
        Context::new(Io::default())
            .compile_str(src, "/", true)
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
    let ours = eval(&expr);
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
    let ours = eval(&expr);
    if !agree(&theirs, &ours) {
        return diverged("catch", src, &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        Outcome::Caught
    } else {
        Outcome::Uncaught
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(src: &str) -> &'static str {
        Outcome::NAMES[check(src).index()]
    }

    #[test]
    fn equal_values() {
        assert_eq!(outcome("{ a = [ 1 2.5 \"x\" ]; }"), "values");
    }

    #[test]
    fn both_reject_parse() {
        assert_eq!(outcome("1 +"), "parse-rejected");
        assert_eq!(outcome("undefined"), "parse-rejected");
        assert_eq!(outcome("~/a"), "parse-rejected");
    }

    #[test]
    fn both_catch() {
        assert_eq!(outcome("throw \"x\""), "caught");
    }

    #[test]
    fn neither_catches() {
        assert_eq!(outcome("abort \"x\""), "uncaught");
    }

    #[test]
    fn ignores_nul_bytes() {
        assert_eq!(outcome("1\0+"), "ignored");
        assert_eq!(outcome("\"a\0b\""), "ignored");
    }

    #[test]
    fn skips_what_nix_cannot_finish() {
        assert_eq!(outcome("let f = x: f x; in f 1"), "skipped");
    }

    #[test]
    fn reports_a_divergence() {
        let Outcome::Diverged(report) = check_with(|_| Ok("2".to_owned()), "1") else {
            panic!("expected a divergence");
        };
        assert!(report.starts_with("step: eval\n"), "{report}");
        assert!(report.contains("--- nix\n1\n--- ours\n2\n"), "{report}");
    }
}
