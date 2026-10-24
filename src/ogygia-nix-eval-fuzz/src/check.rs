//! Comparing ogygia-nix-eval with `nix-instantiate` on one input.

use std::io::Read;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Once;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::time::Duration;
use std::time::Instant;

use ogygia_nix_eval::Context;
use ogygia_nix_eval::Error;
use ogygia_nix_eval::Feature;
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

// `nix-instantiate --eval` only writes what it adds to the store, such as
// `toFile` results and `.drv` files, in read-write mode; without that, or
// with a `dummy://` store, reading them back fails where ogygia-nix-eval
// succeeds.
const NIX_ARGS: &[&str] = &["--read-write-mode", "--option", "pure-eval", "true"];

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
    /// It is not a meaningful comparison: it contains a NUL byte, at which
    /// Nix stops reading its input, or ogygia-nix-eval reached a feature
    /// the fuzzer disables.
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
    /// The two disagree, or ogygia-nix-eval panicked; holds a report of the
    /// step that diverged, the input, and both sides' full output, or of
    /// the panic.
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

    /// The outcome named `name` in [`Outcome::NAMES`], other than
    /// [`Outcome::Diverged`], which holds a report.
    pub fn from_name(name: &str) -> Option<Outcome> {
        Some(match name {
            "ignored" => Outcome::Ignored,
            "parse-rejected" => Outcome::ParseRejected,
            "skipped" => Outcome::Skipped,
            "limited" => Outcome::Limited,
            "values" => Outcome::Value,
            "caught" => Outcome::Caught,
            "uncaught" => Outcome::Uncaught,
            _ => return None,
        })
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

/// An empty chroot store for one Nix run, deleted on drop. Its store
/// directory is still `/nix/store`, so store paths match ogygia-nix-eval's.
struct Store(tempfile::TempDir);

impl Store {
    fn new() -> Store {
        Store(
            tempfile::Builder::new()
                .prefix("store-")
                .tempdir_in(scratch())
                .expect("create store dir"),
        )
    }

    fn uri(&self) -> String {
        format!("local?root={}", self.0.path().display())
    }

    // Nix prints a chroot store's paths at their location on disk, under the
    // root, rather than in the store directory, until NixOS/nix#16625.
    fn unroot(&self, out: &str) -> String {
        out.replace(
            &format!("{}/nix/store", self.0.path().display()),
            "/nix/store",
        )
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Nix makes store paths read-only, which would stop the TempDir
        // removing them once this returns.
        make_writable(self.0.path());
    }
}

fn make_writable(path: &Path) {
    let meta = std::fs::symlink_metadata(path).expect("stat store path");
    if !meta.is_dir() {
        return;
    }
    let mut perms = meta.permissions();
    perms.set_mode(perms.mode() | 0o700);
    std::fs::set_permissions(path, perms).expect("make store path writable");
    for entry in std::fs::read_dir(path).expect("read store dir") {
        make_writable(&entry.expect("read store dir").path());
    }
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
    let s = scratch();
    let store = Store::new();
    let mut cmd = Command::new(nix_instantiate());
    cmd.args(mode)
        .args(NIX_ARGS)
        .arg("--store")
        .arg(store.uri())
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
    if let Budget::Fuzzing = budget {
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
    let stdout = store.unroot(&String::from_utf8_lossy(&stdout.join().unwrap()));
    let stderr = store.unroot(&String::from_utf8_lossy(&stderr.join().unwrap()));
    let status = status?;
    // A SIGKILL not sent above is the kernel's OOM killer.
    if exhausted(&stderr) || status.signal() == Some(libc::SIGKILL) {
        return Err(Outcome::Skipped);
    }
    if limited(&stderr) {
        return Err(Outcome::Limited);
    }
    Ok(if status.success() {
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

fn settings() -> Settings {
    Settings {
        current_system: None,
        nix_path: Vec::new(),
        pure: true,
        // Nix's regular expressions are libstdc++'s `std::regex`, whose
        // semantics are too broken to reasonably match, so inputs that use
        // one are not compared.
        disabled: vec![Feature::Regex],
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

fn panicked(src: &str, expr: &str, panic: &str) -> Outcome {
    Outcome::Diverged(format!(
        "step: panic\n--- input\n{src}\n--- expression\n{expr}\n--- ours\n{panic}\n"
    ))
}

/// The name ogygia-nix-eval gives the threads it runs on, in
/// [`run_with_stack`].
const EVAL_THREAD: &str = "nix-eval";

/// Held while [`catch_ours`] runs ogygia-nix-eval, so that a panic on one of
/// its threads belongs to that call.
static CATCHING: Mutex<()> = Mutex::new(());

/// `Some` while [`catch_ours`] runs ogygia-nix-eval, holding the first panic
/// on one of its threads.
static PANIC: Mutex<Option<Option<String>>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `f`, which runs ogygia-nix-eval, failing with the location and
/// message of a panic on one of ogygia-nix-eval's threads, which is neither
/// reported nor, under libFuzzer, an abort. Any other panic is the
/// fuzzer's own, and propagates as it would without this.
fn catch_ours<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        // Set after libFuzzer's hook, which reports a panic and aborts, so
        // that a panic of ours never reaches that.
        let next = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let ours = std::thread::current().name() == Some(EVAL_THREAD)
                && lock(&PANIC)
                    .as_mut()
                    .map(|panic| panic.get_or_insert_with(|| info.to_string()))
                    .is_some();
            if !ours {
                next(info);
            }
        }));
    });
    let _catching = lock(&CATCHING);
    *lock(&PANIC) = Some(None);
    let result = std::panic::catch_unwind(AssertUnwindSafe(f));
    let panic = lock(&PANIC).take().flatten();
    match (result, panic) {
        (Ok(out), _) => Ok(out),
        (Err(_), Some(panic)) => Err(panic),
        (Err(payload), None) => std::panic::resume_unwind(payload),
    }
}

/// Fails with [`Outcome::Ignored`] when evaluation reached a disabled
/// feature, and with a [divergence](Outcome::Diverged) when it panicked.
fn run_ours(
    eval: impl Fn(&str) -> Result<String, Error>,
    src: &str,
    expr: &str,
) -> Result<Run, Outcome> {
    match catch_ours(|| eval(expr)) {
        Ok(Ok(out)) => Ok(Ok(out)),
        Ok(Err(Error::Disabled(_))) => Err(Outcome::Ignored),
        Ok(Err(Error::Eval(e))) => Ok(Err(e)),
        Err(panic) => Err(panicked(src, expr, &panic)),
    }
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
/// prefix of the input rather than the input. So is one on which
/// ogygia-nix-eval reaches a feature the fuzzer disables.
///
/// A panic in ogygia-nix-eval diverges by itself, at whichever step it
/// happens, ending the check there. ogygia-nix-eval runs before Nix in the
/// parse step, so a panic there runs no Nix at all. A panic in the fuzzer
/// itself propagates.
pub fn check(src: &str, budget: Budget) -> Outcome {
    check_with(
        compile,
        |expr| ogygia_nix_eval::eval_to_string(expr, "/", settings()),
        |mode, expr| run_nix(mode, expr, budget),
        src,
    )
}

fn compile(src: &str) -> Run {
    run_with_stack(|| {
        Context::new(Io::pure())
            .compile_str(src, "/", true)
            .map(|_| String::new())
            .map_err(|e| e.msg)
    })
}

/// [`check`], with `compile` and `eval` standing in for ogygia-nix-eval's
/// parsing and evaluation, and `nix` for running `nix-instantiate`.
fn check_with(
    compile: impl Fn(&str) -> Run,
    eval: impl Fn(&str) -> Result<String, Error>,
    nix: impl Fn(&[&str], &str) -> Result<Run, Outcome>,
    src: &str,
) -> Outcome {
    if src.contains('\0') {
        return Outcome::Ignored;
    }
    let ours = match catch_ours(|| compile(src)) {
        Ok(ours) => ours,
        Err(panic) => return panicked(src, src, &panic),
    };
    let theirs = match nix(&["--parse"], src) {
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
    let theirs = match nix(&["--eval", "--strict"], &expr) {
        Ok(theirs) => theirs,
        Err(outcome) => return outcome,
    };
    let ours = match run_ours(&eval, src, &expr) {
        Ok(ours) => ours,
        Err(outcome) => return outcome,
    };
    if !agree(&theirs, &ours) {
        return diverged("eval", src, &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        return Outcome::Value;
    }

    let expr = format!("builtins.tryEval (builtins.deepSeq (\n{src}\n) null)");
    let theirs = match nix(&["--eval", "--strict"], &expr) {
        Ok(theirs) => theirs,
        Err(outcome) => return outcome,
    };
    let ours = match run_ours(&eval, src, &expr) {
        Ok(ours) => ours,
        Err(outcome) => return outcome,
    };
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
    fn reads_back_what_nix_adds_to_the_store() {
        assert_eq!(
            outcome("builtins.readDir (builtins.filterSource (p: k: false) <nix/.>)"),
            "values"
        );
    }

    #[test]
    fn drv_path_context_takes_the_closure() {
        assert_eq!(
            outcome(
                r#"let
                  b = builtins.toFile "b" "b";
                  x = derivation { name = "x"; system = "s"; builder = "/bin/sh";
                    src = builtins.toFile "a" "${b}"; outputs = [ "out" "dev" ]; };
                  y = derivation { name = "y"; system = "s"; builder = "/bin/sh"; c = x.dev; };
                  f = c: derivation { name = "d"; system = "s"; builder = "/bin/sh"; inherit c; };
                in map (d: { inherit (d) drvPath outPath; }) [ (f x.drvPath) (f y.drvPath) ]"#
            ),
            "values"
        );
    }

    #[test]
    fn append_context_requires_valid_paths() {
        assert_eq!(
            outcome(
                r#"builtins.appendContext "x" { "/nix/store/p1v2hkzib64izpkmp5zr6gn81rrk8pv1-x" = { }; }"#
            ),
            "uncaught"
        );
        assert_eq!(
            outcome(
                r#"let d = derivation { name = "d"; system = "s"; builder = "/bin/sh"; };
                in builtins.appendContext "x" { ${builtins.unsafeDiscardStringContext d.outPath} = throw "t"; }"#
            ),
            "uncaught"
        );
        assert_eq!(
            outcome(
                r#"let d = derivation { name = "d"; system = "s"; builder = "/bin/sh"; };
                in builtins.getContext (builtins.appendContext "x" {
                  ${builtins.unsafeDiscardStringContext d.drvPath} = { allOutputs = true; outputs = [ "out" ]; };
                })"#
            ),
            "values"
        );
        assert_eq!(
            outcome(
                r#"builtins.appendContext "x" {
                  ${builtins.unsafeDiscardStringContext (builtins.toFile "a" "b")} = { outputs = [ (throw "t") ]; };
                }"#
            ),
            "uncaught"
        );
    }

    #[test]
    fn reading_a_path_needs_its_derivation_outputs() {
        let d = r#"let d = derivation { name = "d"; system = "s"; builder = "/bin/sh"; };
            k = builtins.unsafeDiscardStringContext d.drvPath;
            t = builtins.toFile "t" "t";
            c = builtins.substring 0 0 "${d}";
            in"#;
        for expr in [
            "builtins.pathExists d",
            "builtins.pathExists (\"/nonexistent\" + c)",
            "builtins.readFile (t + c)",
            "builtins.readFileType (t + c)",
            "builtins.hashFile \"sha256\" (t + c)",
            "builtins.readDir (\"/\" + c)",
            "import d",
            "scopedImport { } d",
            "builtins.path { path = t + c; }",
            "builtins.filterSource (p: t: true) (t + c)",
            "builtins.findFile [ { path = \"/x\" + c; prefix = \"q\"; } ] \"nix/fetchurl.nix\"",
            "builtins.readFile (builtins.appendContext t { ${k} = { outputs = [ \"x\" ]; }; })",
        ] {
            assert_eq!(outcome(&format!("{d} {expr}")), "uncaught", "{expr}");
        }
        assert_eq!(
            outcome(&format!("{d} builtins.pathExists d.drvPath")),
            "values"
        );
        assert_eq!(
            outcome(&format!(
                "{d} builtins.readFile (builtins.appendContext t {{ ${{k}} = {{ allOutputs = true; }}; }})"
            )),
            "values"
        );
    }

    #[test]
    fn ignores_the_store_root() {
        assert_eq!(outcome("/nix/store/."), "values");
        assert_eq!(
            outcome("/nix/store/p1v2hkzib64izpkmp5zr6gn81rrk8pv1-x/a"),
            "values"
        );
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
    fn ignores_nul_bytes() {
        assert_eq!(outcome("1\0+"), "ignored");
        assert_eq!(outcome("\"a\0b\""), "ignored");
    }

    #[test]
    fn ignores_regex() {
        assert_eq!(outcome("builtins.match \"a\" \"a\""), "ignored");
        assert_eq!(
            outcome("builtins.tryEval (builtins.split \"a\" \"a\")"),
            "ignored"
        );
        assert_eq!(
            outcome("builtins.addErrorContext (throw \"x\") (builtins.match \"a\" \"a\")"),
            "ignored"
        );
        assert_eq!(outcome("builtins.typeOf builtins.match"), "values");
    }

    #[test]
    fn skips_what_nix_cannot_finish() {
        assert_eq!(outcome("let f = x: f x; in f 1"), "skipped");
    }

    #[test]
    fn names_round_trip() {
        for (i, name) in Outcome::NAMES.iter().enumerate() {
            match Outcome::from_name(name) {
                Some(outcome) => assert_eq!(outcome.index(), i),
                None => assert_eq!(*name, "diverged"),
            }
        }
        assert!(Outcome::from_name("diverged").is_none());
        assert!(Outcome::from_name("nonsense").is_none());
    }

    #[test]
    fn reports_a_divergence() {
        let report = report(check_with(
            compile,
            |_| Ok("2".to_owned()),
            |mode, expr| run_nix(mode, expr, Budget::Fuzzing),
            "1",
        ));
        assert!(report.starts_with("step: eval\n"), "{report}");
        assert!(report.contains("--- nix\n1\n--- ours\n2\n"), "{report}");
    }

    fn report(outcome: Outcome) -> String {
        let Outcome::Diverged(report) = outcome else {
            panic!("expected a divergence");
        };
        report
    }

    #[test]
    fn a_panic_in_parsing_diverges_without_nix() {
        let report = report(check_with(
            |_| run_with_stack(|| -> Run { panic!("parse failed") }),
            |_| unreachable!("evaluated"),
            |_, _| unreachable!("ran Nix"),
            "1",
        ));
        let ours = format!("--- ours\npanicked at {}:", file!());
        assert!(
            report.starts_with(&format!(
                "step: panic\n--- input\n1\n--- expression\n1\n{ours}"
            )),
            "{report}"
        );
        assert!(report.ends_with(":\nparse failed\n"), "{report}");
    }

    #[test]
    fn a_panic_in_evaluation_ends_the_check() {
        let report = report(check_with(
            |_| Ok(String::new()),
            |_| run_with_stack(|| -> Result<String, Error> { panic!("eval failed") }),
            |mode, expr| match mode {
                ["--parse"] => Ok(Ok(String::new())),
                _ if expr.starts_with(DEEP_COPY) => Ok(Ok("1".to_owned())),
                _ => unreachable!("ran Nix after the panic"),
            },
            "1",
        ));
        let ours = format!("--- ours\npanicked at {}:", file!());
        assert!(
            report.starts_with(&format!(
                "step: panic\n--- input\n1\n--- expression\n{DEEP_COPY} (\n1\n)\n{ours}"
            )),
            "{report}"
        );
        assert!(report.ends_with(":\neval failed\n"), "{report}");
    }

    #[test]
    fn a_panic_in_the_fuzzer_propagates() {
        // After a panic of ours, which must not be blamed for the others.
        check_with(
            |_| run_with_stack(|| -> Run { panic!("parse failed") }),
            |_| unreachable!("evaluated"),
            |_, _| unreachable!("ran Nix"),
            "1",
        );
        let nix = std::panic::catch_unwind(|| {
            check_with(
                |_| Ok(String::new()),
                |_| unreachable!("evaluated"),
                |_, _| panic!("Nix failed"),
                "1",
            )
        });
        assert_eq!(
            nix.map(drop).unwrap_err().downcast_ref::<&str>(),
            Some(&"Nix failed")
        );
        // Not on one of ogygia-nix-eval's threads.
        let eval = std::panic::catch_unwind(|| {
            check_with(
                |_| Ok(String::new()),
                |_| panic!("eval failed"),
                |_, _| Ok(Ok(String::new())),
                "1",
            )
        });
        assert_eq!(
            eval.map(drop).unwrap_err().downcast_ref::<&str>(),
            Some(&"eval failed")
        );
    }
}
