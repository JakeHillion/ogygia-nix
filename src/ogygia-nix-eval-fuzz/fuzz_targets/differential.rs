//! Differential fuzzing against `nix-instantiate`.
//!
//! An input is Nix source text, so every expression is a possible input and
//! Nix, not a grammar of ours, decides what is valid. Each input is checked
//! in three steps, both evaluators in pure mode:
//!
//! 1. Parse: whether ogygia-nix-eval compiles it must match whether
//!    `nix-instantiate --parse` accepts it, undefined variables included.
//! 2. Evaluate: the deeply evaluated values must be equal, or both sides
//!    must fail.
//! 3. When both fail, `builtins.tryEval` must catch the failure on both
//!    sides or on neither. Error messages are not compared.
//!
//! Inputs where Nix runs out of time, memory or stack say nothing about
//! equivalence and are skipped before ogygia-nix-eval sees them. Both
//! evaluators are pointed at a proxy that refuses connections, so fetches
//! fail without touching the network.
//!
//! To show whether a run is still doing real work, every 15 seconds each
//! process reports how many inputs had each outcome since its last report,
//! as a Unix time followed by name and count pairs. Reports go to standard
//! error, or are appended to `$OGYGIA_NIX_EVAL_FUZZ_STATS` when it is set,
//! which is how to see them from `-fork` mode's child processes.
//!
//! Mutations are libFuzzer's own byte-level ones, which can turn any input
//! into any other, alongside ones that splice nodes of rnix's syntax tree
//! within and between inputs. The package ships seed inputs and a dictionary
//! of Nix tokens and builtin names:
//!
//! ```text
//! ogygia-nix-eval-fuzz-differential corpus $pkg/share/ogygia-nix-eval-fuzz/seeds \
//!     -dict=$pkg/share/ogygia-nix-eval-fuzz/nix.dict
//! ```
//!
//! `nix-instantiate` is `$OGYGIA_NIX_EVAL_NIX_INSTANTIATE`, else the path
//! baked in at build time from `$OGYGIA_NIX_INSTANTIATE_BIN`, else found on
//! `PATH`.

#![no_main]

use std::io::Read;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;

use libfuzzer_sys::fuzz_crossover;
use libfuzzer_sys::fuzz_mutator;
use libfuzzer_sys::fuzz_target;
use ogygia_nix_eval::Context;
use ogygia_nix_eval::Io;
use ogygia_nix_eval::Settings;
use ogygia_nix_eval::run_with_stack;
use rnix::TextRange;

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

#[derive(Clone, Copy)]
enum Outcome {
    NotUtf8,
    ParseRejected,
    Skipped,
    Value,
    Caught,
    Uncaught,
}

const OUTCOMES: [&str; 6] = [
    "not-utf8",
    "parse-rejected",
    "skipped",
    "values",
    "caught",
    "uncaught",
];

const REPORT_EVERY: Duration = Duration::from_secs(15);

fn record(outcome: Outcome) {
    static PENDING: Mutex<Option<(Instant, [u64; 6])>> = Mutex::new(None);
    let mut pending = PENDING.lock().unwrap();
    let (since, counts) = pending.get_or_insert_with(|| (Instant::now(), [0; 6]));
    counts[outcome as usize] += 1;
    if since.elapsed() < REPORT_EVERY {
        return;
    }
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut line = time.to_string();
    for (name, n) in OUTCOMES.iter().zip(&*counts) {
        line += &format!(" {name} {n}");
    }
    line.push('\n');
    match std::env::var_os("OGYGIA_NIX_EVAL_FUZZ_STATS") {
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

/// Standard output on success, standard error on failure.
type Run = Result<String, String>;

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

fn diverged(step: &str, expr: &str, theirs: &Run, ours: &Run) -> ! {
    panic!(
        "nix and ogygia-nix-eval disagree on {step} of\n{expr}\n--- nix:\n{}\n--- ours:\n{}",
        show(theirs),
        show(ours)
    )
}

/// Both sides succeed with the same output, or both fail.
fn agree(theirs: &Run, ours: &Run) -> bool {
    match (theirs, ours) {
        (Ok(a), Ok(b)) => a == b,
        (Err(_), Err(_)) => true,
        _ => false,
    }
}

fn check(data: &[u8]) -> Outcome {
    let Ok(src) = std::str::from_utf8(data) else {
        return Outcome::NotUtf8;
    };

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
        diverged("parsing", src, &theirs, &ours);
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
        diverged("evaluation", &expr, &theirs, &ours);
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
        diverged("catching the error", &expr, &theirs, &ours);
    }
    if ours.is_ok() {
        Outcome::Caught
    } else {
        Outcome::Uncaught
    }
}

fuzz_target!(
    init: {
        for var in PROXY_VARS {
            // SAFETY: libFuzzer initialises the target before starting any
            // thread that could read the environment.
            unsafe { std::env::set_var(var, DEAD_PROXY) };
        }
        for var in ["no_proxy", "NO_PROXY"] {
            // SAFETY: as above.
            unsafe { std::env::remove_var(var) };
        }
    },
    |data: &[u8]| record(check(data))
);

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
