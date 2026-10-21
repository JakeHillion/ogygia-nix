//! The `ogygia-nix-eval-fuzz` command; README.md describes its use.

use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_int;
use std::fs::File;
use std::io::Write;
use std::num::NonZero;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::Duration;

use clap::Parser;
use libfuzzer_sys::fuzz_crossover;
use libfuzzer_sys::fuzz_mutator;
use libfuzzer_sys::fuzz_target;
use ogygia_nix_eval_fuzz::Budget;
use ogygia_nix_eval_fuzz::Outcome;
use ogygia_nix_eval_fuzz::Summary;

/// Differential fuzzing of ogygia-nix-eval against Nix. Arguments that are
/// not a subcommand are passed to libFuzzer.
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Recheck the findings in DIR, then fuzz until stopped.
    Run {
        dir: PathBuf,
        /// Findings to recheck and fuzzing processes to run at once;
        /// defaults to one per CPU.
        #[arg(long)]
        jobs: Option<NonZero<usize>>,
    },
    /// Check every finding in DIR/findings again, keeping only those that
    /// still diverge. Must not run while DIR is being fuzzed.
    Recheck {
        dir: PathBuf,
        /// Findings to recheck at once; defaults to one per CPU.
        #[arg(long)]
        jobs: Option<NonZero<usize>>,
    },
    /// Compare ogygia-nix-eval with Nix on the Nix expression in FILE.
    Check { file: PathBuf },
}

struct Options {
    findings: Option<PathBuf>,
    stats: Option<PathBuf>,
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

/// The original standard error, once [`quiet`] has pointed descriptor 2 at
/// /dev/null.
static LOG: OnceLock<File> = OnceLock::new();

/// `eprintln!`, to [`LOG`] once it is set.
macro_rules! report {
    ($($arg:tt)*) => {
        match LOG.get() {
            Some(mut log) => writeln!(log, $($arg)*),
            None => writeln!(std::io::stderr(), $($arg)*),
        }
        .expect("writing to standard error")
    };
}

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let options = OPTIONS.get().expect("options are set before fuzzing");
    let outcome = ogygia_nix_eval_fuzz::check(src, Budget::Fuzzing);
    if let Outcome::Diverged(report) = &outcome {
        match &options.findings {
            Some(dir) => {
                ogygia_nix_eval_fuzz::record(dir, src, report).expect("recording a finding")
            }
            None => panic!("ogygia-nix-eval and Nix disagree\n{report}"),
        }
    }
    ogygia_nix_eval_fuzz::count(&outcome, options.stats.as_deref());
});

unsafe extern "C" {
    fn LLVMFuzzerRunDriver(
        argc: *mut c_int,
        argv: *mut *mut *mut c_char,
        callback: extern "C" fn(*const u8, usize) -> c_int,
    ) -> c_int;
}

extern "C" fn test_one_input(data: *const u8, size: usize) -> c_int {
    // SAFETY: libFuzzer passes a buffer of `size` bytes.
    unsafe { libfuzzer_sys::test_input_wrap(data, size) }
}

/// Hand `args`, program name first, to libFuzzer.
fn fuzz(args: Vec<String>) -> ExitCode {
    let flag = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(name))
            .map(PathBuf::from)
    };
    let options = Options {
        findings: flag("--findings="),
        stats: flag("--stats="),
    };
    if OPTIONS.set(options).is_err() {
        unreachable!("fuzzing starts once");
    }
    let args: Vec<CString> = args
        .into_iter()
        .map(|a| CString::new(a).expect("arguments cannot contain NUL"))
        .collect();
    let mut argv: Vec<*mut c_char> = args
        .iter()
        .map(|a| a.as_ptr().cast_mut())
        .chain([std::ptr::null_mut()])
        .collect();
    let mut argc = c_int::try_from(args.len()).expect("argument count fits in an int");
    let mut argv = argv.as_mut_ptr();
    // SAFETY: argv is a NULL-terminated array of argc C strings, all of
    // which outlive the call.
    let code = unsafe { LLVMFuzzerRunDriver(&mut argc, &mut argv, test_one_input) };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn recheck(findings: &Path, jobs: NonZero<usize>) -> bool {
    match ogygia_nix_eval_fuzz::recheck(
        findings,
        jobs,
        |src| ogygia_nix_eval_fuzz::check(src, Budget::Unlimited),
        |line| report!("{line}"),
    ) {
        Ok(r) => {
            report!(
                "rechecked findings: {} kept, {} removed, {} inconclusive",
                r.kept,
                r.removed,
                r.inconclusive
            );
            true
        }
        Err(e) => {
            report!("rechecking {}: {e}", findings.display());
            false
        }
    }
}

/// Point descriptor 2 at /dev/null, returning the original standard error.
fn silence_stderr() -> std::io::Result<File> {
    let original = std::io::stderr().as_fd().try_clone_to_owned()?;
    let null = File::options().write(true).open("/dev/null")?;
    // SAFETY: dup2 only replaces descriptor 2, which stays open.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(original.into())
}

/// Send standard error to [`LOG`], and descriptor 2 to /dev/null:
/// libFuzzer reports every job, and evaluation traces and warns with bytes
/// of the input, so only our own lines go to standard error.
fn quiet() -> bool {
    match silence_stderr() {
        Ok(log) => {
            LOG.set(log).expect("standard error is silenced once");
            std::panic::set_hook(Box::new(|info| report!("{info}")));
            true
        }
        Err(e) => {
            eprintln!("silencing standard error: {e}");
            false
        }
    }
}

fn run(dir: &Path, jobs: NonZero<usize>) -> ExitCode {
    let Some(seeds) = option_env!("OGYGIA_NIX_EVAL_FUZZ_SEEDS") else {
        eprintln!("this build has no seed inputs; build it with Nix");
        return ExitCode::FAILURE;
    };
    let revision = option_env!("OGYGIA_NIX_EVAL_FUZZ_REV").unwrap_or("unknown");
    let findings = dir.join("findings");
    let corpus = dir.join("corpus");
    let this_run = dir.join("runs").join(revision);
    for d in [&corpus, &this_run.join("artifacts")] {
        if let Err(e) = std::fs::create_dir_all(d) {
            report!("creating {}: {e}", d.display());
            return ExitCode::FAILURE;
        }
    }
    if !quiet() {
        return ExitCode::FAILURE;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            report!("locating this program: {e}");
            return ExitCode::FAILURE;
        }
    };
    // libFuzzer limits the peak memory use of this process, which every
    // process it starts inherits, so the recheck, which can use far more,
    // runs in a process of its own.
    let rechecked = LOG
        .get()
        .expect("standard error is silenced")
        .try_clone()
        .and_then(|log| {
            std::process::Command::new(&exe)
                .arg("recheck")
                .arg(dir)
                .arg(format!("--jobs={jobs}"))
                .stderr(log)
                .status()
        });
    match rechecked {
        Ok(status) if status.success() => {}
        Ok(_) => return ExitCode::FAILURE,
        Err(e) => {
            report!("running the recheck: {e}");
            return ExitCode::FAILURE;
        }
    }
    let mut summary = match Summary::new(&this_run.join("stats"), &findings) {
        Ok(summary) => summary,
        Err(e) => {
            report!("reading {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(60));
            for line in summary.update().expect("summarising the run") {
                report!("{line}");
            }
        }
    });
    let path = |p: &Path| p.display().to_string();
    fuzz(vec![
        path(&exe),
        format!("-fork={jobs}"),
        "-ignore_crashes=1".to_owned(),
        "-ignore_timeouts=1".to_owned(),
        "-ignore_ooms=1".to_owned(),
        // Nix gets 10 seconds, so a timeout means ours is far slower.
        "-timeout=60".to_owned(),
        format!("-artifact_prefix={}/", path(&this_run.join("artifacts"))),
        format!("-dict={seeds}/nix.dict"),
        format!("--findings={}", path(&findings)),
        format!("--stats={}", path(&this_run.join("stats"))),
        path(&corpus),
        format!("{seeds}/seeds"),
    ])
}

/// `jobs`, defaulting to one per CPU.
fn or_cpus(jobs: Option<NonZero<usize>>) -> NonZero<usize> {
    jobs.unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZero::<usize>::MIN))
}

fn check(file: &Path) -> ExitCode {
    let src = match std::fs::read(file).map(String::from_utf8) {
        Ok(Ok(src)) => src,
        Ok(Err(_)) => {
            eprintln!("{} is not UTF-8", file.display());
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("reading {}: {e}", file.display());
            return ExitCode::FAILURE;
        }
    };
    match ogygia_nix_eval_fuzz::check(&src, Budget::Unlimited) {
        Outcome::Diverged(report) => {
            print!("{report}");
            ExitCode::FAILURE
        }
        outcome => {
            println!("{}", Outcome::NAMES[outcome.index()]);
            ExitCode::SUCCESS
        }
    }
}

fn main() -> ExitCode {
    // SAFETY: no other thread has started yet.
    unsafe { ogygia_nix_eval_fuzz::block_network() };
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let subcommand = args.get(1).map(String::as_str);
    if !matches!(
        subcommand,
        Some("run" | "recheck" | "check" | "help" | "-h" | "--help")
    ) {
        return fuzz(args);
    }
    match Cli::parse().command {
        Command::Run { dir, jobs } => run(&dir, or_cpus(jobs)),
        Command::Recheck { dir, jobs } => {
            if quiet() && recheck(&dir.join("findings"), or_cpus(jobs)) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::Check { file } => check(&file),
    }
}

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    ogygia_nix_eval_fuzz::mutate(data, size, max_size, seed, libfuzzer_sys::fuzzer_mutate)
});

fuzz_crossover!(|data1: &[u8], data2: &[u8], out: &mut [u8], seed: u32| {
    ogygia_nix_eval_fuzz::crossover(data1, data2, out, seed)
});
