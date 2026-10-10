//! Comparing ogygia-nix-eval with `nix-instantiate` on one input.

use std::ffi::CStr;
use std::ffi::CString;
use std::io::Read;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
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

/// How long Nix may spend on one expression of a new input.
const NIX_TIMEOUT: Duration = Duration::from_secs(10);

/// Address space Nix may use on a new input.
const NIX_MEMORY: libc::rlim_t = 1 << 30;

/// What Nix may use on an input before [`check`] skips it.
#[derive(Clone, Copy)]
pub enum Budget {
    /// 10 seconds per expression and 1 GiB of address space, for new inputs
    /// while fuzzing.
    Fuzzing,
    /// No limit, for findings: each already finished within
    /// [`Budget::Fuzzing`] once, so a limit would only make the result
    /// depend on how busy the machine is.
    Unlimited,
}

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
    "--read-write-mode",
    "--store",
    "local",
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
    /// Nix ran out of time, memory or stack, or was killed.
    Skipped,
    /// Nix hit a fixed limit of its implementation, which it hits on every
    /// run, such as the size of a compiled regular expression.
    Limited,
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
    pub const NAMES: [&str; 8] = [
        "ignored",
        "parse-rejected",
        "skipped",
        "limited",
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
            Outcome::Limited => 3,
            Outcome::Value => 4,
            Outcome::Caught => 5,
            Outcome::Uncaught => 6,
            Outcome::Diverged(_) => 7,
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

/// Fails with [`Outcome::Skipped`] or [`Outcome::Limited`] when what Nix
/// did says nothing about equivalence.
fn run_nix(mode: &[&str], expr: &str, budget: Budget) -> Result<Run, Outcome> {
    let store = tempfile::Builder::new()
        .prefix("store-")
        .tempdir_in(scratch())
        .expect("creating a Nix store")
        .keep();
    let run = run_nix_in(&store, mode, expr, budget);
    remove_store(&store);
    run
}

/// Delete what a run of Nix wrote, whose directories it made read-only.
fn remove_store(store: &Path) {
    fn make_writable(path: &Path) -> std::io::Result<()> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_dir() {
            return Ok(());
        }
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o700);
        std::fs::set_permissions(path, perms)?;
        for entry in std::fs::read_dir(path)? {
            make_writable(&entry?.path())?;
        }
        Ok(())
    }
    make_writable(store).expect("making a Nix store writable");
    std::fs::remove_dir_all(store).expect("removing a Nix store");
}

/// Write `data` to the file at `path`, between fork and exec.
///
/// # Safety
///
/// As [`CommandExt::pre_exec`]: only async-signal-safe calls.
unsafe fn write_file(path: &CStr, data: &CStr) -> std::io::Result<()> {
    let data = data.to_bytes();
    // SAFETY: open, write and close are async-signal-safe, and both pointers
    // are valid for the lengths given.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let written = libc::write(fd, data.as_ptr().cast(), data.len());
        let err = std::io::Error::last_os_error();
        libc::close(fd);
        if written != data.len() as isize {
            return Err(err);
        }
    }
    Ok(())
}

fn run_nix_in(store: &Path, mode: &[&str], expr: &str, budget: Budget) -> Result<Run, Outcome> {
    // Nix gets user and mount namespaces in which /nix/store is overlaid by an
    // empty layer in `store`, with a database there too. It writes and reads
    // back paths at /nix/store as on any system, as a store elsewhere would
    // show in path values, and the host store and other runs stay untouched.
    let s = scratch();
    for dir in ["upper", "work", "state"] {
        std::fs::create_dir(store.join(dir)).expect("creating a Nix store");
    }
    let dir = store.to_str().expect("a UTF-8 scratch directory");
    assert!(
        !dir.contains([',', ':', '\\']),
        "{dir} cannot be given to overlayfs"
    );
    let overlay = CString::new(format!(
        "lowerdir=/nix/store,upperdir={dir}/upper,workdir={dir}/work"
    ))
    .unwrap();
    // Nix keeps our ids, so it does not act as root.
    // SAFETY: getuid and getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = CString::new(format!("{uid} {uid} 1")).unwrap();
    let gid_map = CString::new(format!("{gid} {gid} 1")).unwrap();
    let limit = match budget {
        Budget::Fuzzing => Some(libc::rlimit {
            rlim_cur: NIX_MEMORY,
            rlim_max: NIX_MEMORY,
        }),
        Budget::Unlimited => None,
    };
    let mut cmd = Command::new(nix_instantiate());
    cmd.args(mode)
        .args(NIX_ARGS)
        // On standard input, the expression cannot be mistaken for a flag and
        // may contain NUL bytes.
        .arg("-")
        .current_dir("/")
        .env_clear()
        .env("HOME", s)
        .env("NIX_STATE_DIR", store.join("state"))
        .env("NIX_CONF_DIR", s.join("conf"))
        .env("NIX_LOG_DIR", s.join("log"))
        .env("XDG_CACHE_HOME", s.join("cache"))
        .envs(PROXY_VARS.iter().map(|v| (v, DEAD_PROXY)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: the closure makes only async-signal-safe calls, on memory it
    // owns, and the child is single-threaded, as unshare(CLONE_NEWUSER)
    // requires.
    unsafe {
        cmd.pre_exec(move || {
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            write_file(c"/proc/self/setgroups", c"deny")?;
            write_file(c"/proc/self/uid_map", &uid_map)?;
            write_file(c"/proc/self/gid_map", &gid_map)?;
            if libc::mount(
                c"overlay".as_ptr(),
                c"/nix/store".as_ptr(),
                c"overlay".as_ptr(),
                0,
                overlay.as_ptr().cast(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(limit) = limit
                && libc::setrlimit(libc::RLIMIT_AS, &limit) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
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
    let deadline = match budget {
        Budget::Fuzzing => Some(Instant::now() + NIX_TIMEOUT),
        Budget::Unlimited => None,
    };
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting for nix-instantiate") {
            break Ok(status);
        }
        if deadline.is_some_and(|d| Instant::now() > d) {
            child.kill().expect("killing nix-instantiate");
            child.wait().expect("waiting for nix-instantiate");
            break Err(Outcome::Skipped);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    stdin.join().unwrap();
    let stdout = stdout.join().unwrap();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap()).into_owned();
    let status = status?;
    // A SIGKILL not sent above is the kernel's OOM killer.
    if exhausted(&stderr) || status.signal() == Some(libc::SIGKILL) {
        return Err(Outcome::Skipped);
    }
    if limited(&stderr) {
        return Err(Outcome::Limited);
    }
    Ok(if status.success() {
        let stdout = String::from_utf8_lossy(&stdout);
        Ok(stdout.strip_suffix('\n').unwrap_or(&stdout).to_owned())
    } else {
        Err(stderr)
    })
}

/// Whether Nix's standard error says it ran out of memory or stack.
fn exhausted(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    [
        "out of memory",
        "bad_alloc",
        "stack overflow",
        "max-call-depth",
    ]
    .iter()
    .any(|e| stderr.contains(e))
}

/// Whether Nix's standard error says it hit a fixed limit of its
/// implementation.
fn limited(stderr: &str) -> bool {
    stderr.contains("memory limit exceeded by regular expression")
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
/// Nix runs first at each step, within `budget`. An input on which it runs
/// out of time, memory or stack is [skipped](Outcome::Skipped), and one on which it hits
/// a fixed limit of its implementation is [limited](Outcome::Limited).
/// An input containing a NUL byte is [ignored](Outcome::Ignored): Nix
/// stops reading at the first one outside a string, so it evaluates a
/// prefix of the input rather than the input.
pub fn check(src: &str, budget: Budget) -> Outcome {
    check_with(
        |expr| ogygia_nix_eval::eval_to_string(expr, "/", pure()),
        src,
        budget,
    )
}

/// [`check`], with `eval` standing in for ogygia-nix-eval's evaluation.
fn check_with(eval: impl Fn(&str) -> Run, src: &str, budget: Budget) -> Outcome {
    if src.contains('\0') {
        return Outcome::Ignored;
    }
    let ours: Run = run_with_stack(|| {
        Context::new(Io::pure())
            .compile_str(src, "/", true)
            .map(|_| String::new())
            .map_err(|e| e.msg)
    });
    let theirs = match run_nix(&["--parse"], src, budget) {
        Ok(theirs) => theirs,
        Err(outcome) => return outcome,
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
    let theirs = match run_nix(&["--eval", "--strict"], &expr, budget) {
        Ok(theirs) => theirs,
        Err(outcome) => return outcome,
    };
    let ours = eval(&expr);
    if !agree(&theirs, &ours) {
        return diverged("eval", src, &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        return Outcome::Value;
    }

    let expr = format!("builtins.tryEval (builtins.deepSeq (\n{src}\n) null)");
    let theirs = match run_nix(&["--eval", "--strict"], &expr, budget) {
        Ok(theirs) => theirs,
        Err(outcome) => return outcome,
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
        Outcome::NAMES[check(src, Budget::Fuzzing).index()]
    }

    #[test]
    fn gc_out_of_memory_is_exhaustion() {
        assert!(exhausted(
            "GC Warning: Out of Memory! Heap size: 434 MiB. Returning NULL!\n\
             Insufficient space for initial table allocation\n"
        ));
    }

    #[test]
    fn limits_what_nix_never_finishes() {
        assert_eq!(
            outcome(
                "builtins.match (\"(a{2}b){2}\" + \
                 builtins.concatStringsSep \"\" (builtins.genList (_: \"a\") 99970)) \"\""
            ),
            "limited"
        );
    }

    #[test]
    fn equal_values() {
        assert_eq!(outcome("{ a = [ 1 2.5 \"x\" ]; }"), "values");
    }

    #[test]
    fn keeps_trailing_whitespace_in_paths() {
        assert_eq!(outcome("/a + \"b \""), "values");
    }

    #[test]
    fn both_reject_parse() {
        assert_eq!(outcome("1 +"), "parse-rejected");
        assert_eq!(outcome("undefined"), "parse-rejected");
        assert_eq!(outcome("~/a"), "parse-rejected");
        assert_eq!(outcome("__currentTime"), "parse-rejected");
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
    fn forbids_paths_outside_mounts() {
        assert_eq!(outcome("\"${/bin/sh}\""), "uncaught");
        assert_eq!(
            outcome("builtins.toJSON { outPath = /bin/sh; }"),
            "uncaught"
        );
        assert_eq!(outcome("builtins.readFile /nonexistent"), "uncaught");
        assert_eq!(outcome("builtins.pathExists /bin/sh"), "values");
        assert_eq!(
            outcome("builtins.findFile [ { path = \"/bin\"; prefix = \"\"; } ] \"sh\""),
            "uncaught"
        );
    }

    #[test]
    fn shows_the_store_directory_without_a_trailing_slash() {
        assert_eq!(outcome("/nix/store/."), "values");
        assert_eq!(outcome("builtins.toXML [ /nix/store /nix ]"), "values");
    }

    #[test]
    fn reads_back_what_nix_writes() {
        assert_eq!(
            outcome("builtins.readFile \"${<nix/fetchurl.nix>}\" != \"\""),
            "values"
        );
    }

    #[test]
    fn removes_read_only_stores() {
        let store = tempfile::tempdir_in(scratch()).unwrap().keep();
        let dir = store.join("upper/x");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), "").unwrap();
        for d in [&dir, &store.join("upper")] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        remove_store(&store);
        assert!(!store.exists());
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
        let Outcome::Diverged(report) = check_with(|_| Ok("2".to_owned()), "1", Budget::Fuzzing)
        else {
            panic!("expected a divergence");
        };
        assert!(report.starts_with("step: eval\n"), "{report}");
        assert!(report.contains("--- nix\n1\n--- ours\n2\n"), "{report}");
    }
}
