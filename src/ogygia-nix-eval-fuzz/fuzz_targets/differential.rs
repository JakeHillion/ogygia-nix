//! Differential fuzzing of ogygia-nix-eval against Nix.
//!
//! `run DIR` is the long-running mode. It rechecks the findings in
//! `DIR/findings` with this build, then fuzzes until stopped: new inputs go
//! in `DIR/corpus`, each input that diverges in `DIR/findings`, and
//! per-revision outcome counts and crashes of either evaluator in
//! `DIR/runs/<revision>`. `recheck DIR` does only the first part, and
//! `check FILE` compares the two evaluators on one input.
//!
//! Any other arguments go to libFuzzer, which is how `cargo fuzz run`
//! invokes it; then a divergence aborts with its report unless
//! `--findings=DIR` says where to record it, and `--stats=FILE` appends
//! outcome counts to FILE rather than standard error.
//!
//! Mutations are libFuzzer's own byte-level ones, which can turn any input
//! into any other, alongside ones that splice nodes of rnix's syntax tree
//! within and between inputs.

use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_int;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::Parser;
use libfuzzer_sys::fuzz_crossover;
use libfuzzer_sys::fuzz_mutator;
use libfuzzer_sys::fuzz_target;
use ogygia_nix_eval::run_with_stack;
use ogygia_nix_eval_fuzz::Outcome;
use rnix::TextRange;

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
        /// Fuzzing processes to run; defaults to all but 8 of the CPUs.
        #[arg(long)]
        jobs: Option<usize>,
    },
    /// Check every finding in DIR/findings again, keeping only those that
    /// still diverge. Must not run while DIR is being fuzzed.
    Recheck { dir: PathBuf },
    /// Compare ogygia-nix-eval with Nix on the Nix expression in FILE.
    Check { file: PathBuf },
}

struct Options {
    findings: Option<PathBuf>,
    stats: Option<PathBuf>,
}

static OPTIONS: OnceLock<Options> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let options = OPTIONS.get().expect("options are set before fuzzing");
    let outcome = ogygia_nix_eval_fuzz::check(src);
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

fn recheck(findings: &Path) -> bool {
    match ogygia_nix_eval_fuzz::recheck(findings) {
        Ok(r) => {
            eprintln!(
                "rechecked findings: {} kept, {} removed, {} inconclusive",
                r.kept, r.removed, r.inconclusive
            );
            true
        }
        Err(e) => {
            eprintln!("rechecking {}: {e}", findings.display());
            false
        }
    }
}

fn run(dir: &Path, jobs: Option<usize>) -> ExitCode {
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
            eprintln!("creating {}: {e}", d.display());
            return ExitCode::FAILURE;
        }
    }
    if !recheck(&findings) {
        return ExitCode::FAILURE;
    }
    let jobs = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(8).max(1))
    });
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("locating this program: {e}");
            return ExitCode::FAILURE;
        }
    };
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
    match ogygia_nix_eval_fuzz::check(&src) {
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
        Command::Run { dir, jobs } => run(&dir, jobs),
        Command::Recheck { dir } => {
            if recheck(&dir.join("findings")) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Command::Check { file } => check(&file),
    }
}

struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Rng {
        Rng(seed | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        // xorshift32
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as usize % n
    }
}

/// The ranges of every node and token in rnix's syntax tree of `text`.
fn elements(text: &str) -> Vec<TextRange> {
    run_with_stack(|| {
        rnix::Root::parse(text)
            .syntax()
            .descendants_with_tokens()
            .map(|e| e.text_range())
            .collect()
    })
}

fn splice(text: &str, range: TextRange, with: &str) -> String {
    let (start, end) = (usize::from(range.start()), usize::from(range.end()));
    [&text[..start], with, &text[end..]].concat()
}

/// Replace, delete, copy or swap whole nodes and tokens of `text`.
fn mutate_tree(text: &str, rng: &mut Rng) -> String {
    let ranges = elements(text);
    let a = ranges[rng.below(ranges.len())];
    let b = ranges[rng.below(ranges.len())];
    let piece = &text[b];
    match rng.below(4) {
        0 => splice(text, a, piece),
        1 => splice(text, a, ""),
        2 => splice(text, TextRange::empty(a.end()), &format!(" {piece}")),
        _ => {
            let (first, second) = if a.start() <= b.start() {
                (a, b)
            } else {
                (b, a)
            };
            if first.end() > second.start() {
                return splice(text, a, piece);
            }
            let between = TextRange::new(first.end(), second.start());
            [
                &text[..usize::from(first.start())],
                &text[second],
                &text[between],
                &text[first],
                &text[usize::from(second.end())..],
            ]
            .concat()
        }
    }
}

fuzz_mutator!(|data: &mut [u8], size: usize, max_size: usize, seed: u32| {
    let mut rng = Rng::new(seed);
    if rng.below(2) == 0
        && let Ok(text) = std::str::from_utf8(&data[..size])
    {
        let out = mutate_tree(text, &mut rng);
        if out.len() <= max_size {
            data[..out.len()].copy_from_slice(out.as_bytes());
            return out.len();
        }
    }
    libfuzzer_sys::fuzzer_mutate(data, size, max_size)
});

fuzz_crossover!(|data1: &[u8], data2: &[u8], out: &mut [u8], seed: u32| {
    let mut rng = Rng::new(seed);
    // Put a node or token of one input in place of one of the other's.
    let spliced = match (std::str::from_utf8(data1), std::str::from_utf8(data2)) {
        (Ok(a), Ok(b)) => {
            let to = elements(a);
            let from = elements(b);
            let piece = &b[from[rng.below(from.len())]];
            splice(a, to[rng.below(to.len())], piece).into_bytes()
        }
        _ => {
            let cut1 = rng.below(data1.len() + 1);
            let cut2 = rng.below(data2.len() + 1);
            [&data1[..cut1], &data2[cut2..]].concat()
        }
    };
    let len = spliced.len().min(out.len());
    out[..len].copy_from_slice(&spliced[..len]);
    len
});
