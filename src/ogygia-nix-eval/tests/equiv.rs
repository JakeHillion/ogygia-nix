//! Equivalence tests: every `tests/cases/**/*.nix` file is evaluated by this
//! crate and by a pinned `nix-instantiate`, and the deeply forced results must
//! print identically (or both evaluations must fail).
//!
//! Adding a test is adding a file. A case may start with directive comments:
//!
//! - `# nix-path: <prefix>=<path>` adds a search path entry. `@NIXPKGS@` in
//!   the path is replaced by `$OGYGIA_NIX_EVAL_NIXPKGS`; the case is ignored
//!   when that is unset.
//!
//! The Nix binary is `$OGYGIA_NIX_EVAL_NIX_INSTANTIATE`, else the one baked in
//! at build time through `OGYGIA_NIX_INSTANTIATE_BIN`, else `nix-instantiate`
//! from `PATH`. It must be the pinned version unless
//! `$OGYGIA_NIX_EVAL_ANY_NIX` is set.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use libtest_mimic::Arguments;
use libtest_mimic::Completion;
use libtest_mimic::Failed;
use libtest_mimic::Trial;
use ogygia_nix_eval::Settings;

const PINNED_NIX_VERSION: &str = "2.34.8";

/// Rebuilds every set and list so that Nix's printer never abbreviates a
/// value it has already printed as `«repeated»`, which depends on sharing
/// that is an implementation detail.
const DEEP_COPY: &str = "let dc = v: if builtins.isAttrs v then builtins.mapAttrs (_: dc) v \
                         else if builtins.isList v then map dc v else v; in dc";

fn nix_instantiate() -> String {
    std::env::var("OGYGIA_NIX_EVAL_NIX_INSTANTIATE")
        .ok()
        .or_else(|| option_env!("OGYGIA_NIX_INSTANTIATE_BIN").map(str::to_owned))
        .unwrap_or_else(|| "nix-instantiate".to_owned())
}

/// A scratch directory for Nix's state, shared by all cases.
fn scratch() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("ogygia-nix-eval-equiv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    })
}

fn nix_command() -> Command {
    let mut cmd = Command::new(nix_instantiate());
    let s = scratch();
    cmd.env("HOME", s)
        .env("NIX_STATE_DIR", s.join("state"))
        .env("NIX_CONF_DIR", s.join("conf"))
        .env("NIX_LOG_DIR", s.join("log"))
        .env("XDG_CACHE_HOME", s.join("cache"))
        .env_remove("NIX_PATH");
    cmd
}

fn check_nix_version() -> Result<(), String> {
    if std::env::var_os("OGYGIA_NIX_EVAL_ANY_NIX").is_some() {
        return Ok(());
    }
    let out = nix_command()
        .arg("--version")
        .output()
        .map_err(|e| format!("running {}: {e}", nix_instantiate()))?;
    let version = String::from_utf8_lossy(&out.stdout);
    if !version.contains(PINNED_NIX_VERSION) {
        return Err(format!(
            "equivalence tests need Nix {PINNED_NIX_VERSION}, found: {}",
            version.trim()
        ));
    }
    Ok(())
}

struct Case {
    path: PathBuf,
    nix_path: Vec<(String, String)>,
}

/// Parse the directives of a case; `None` if it needs something unavailable.
fn load_case(path: &Path) -> Result<Option<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut nix_path = Vec::new();
    for line in text.lines() {
        let Some(comment) = line.strip_prefix('#') else {
            break;
        };
        if let Some(entry) = comment.trim().strip_prefix("nix-path:") {
            let (prefix, p) = entry
                .trim()
                .split_once('=')
                .ok_or_else(|| format!("bad nix-path directive: {line}"))?;
            let p = if p.contains("@NIXPKGS@") {
                match std::env::var("OGYGIA_NIX_EVAL_NIXPKGS") {
                    Ok(n) => p.replace("@NIXPKGS@", &n),
                    Err(_) => return Ok(None),
                }
            } else {
                p.to_owned()
            };
            nix_path.push((prefix.to_owned(), p));
        }
    }
    Ok(Some(Case {
        path: path.to_owned(),
        nix_path,
    }))
}

fn run_nix(case: &Case, expr: &str) -> Result<String, String> {
    let mut cmd = nix_command();
    cmd.args([
        "--eval",
        "--strict",
        "--readonly-mode",
        "--store",
        "dummy://",
    ]);
    for (prefix, p) in &case.nix_path {
        cmd.arg("-I").arg(format!("{prefix}={p}"));
    }
    cmd.arg("--expr").arg(expr);
    let out = cmd.output().map_err(|e| format!("running nix: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn run_ours(case: &Case, expr: &str) -> Result<String, String> {
    let settings = Settings {
        nix_path: case.nix_path.clone(),
        ..Settings::default()
    };
    let dir = case.path.parent().unwrap().to_string_lossy().into_owned();
    ogygia_nix_eval::eval_to_string(expr, &dir, settings)
}

fn run_case(case: &Case) -> Result<(), Failed> {
    let expr = format!("{DEEP_COPY} (import {})", case.path.display());
    let theirs = run_nix(case, &expr);
    let ours = run_ours(case, &expr);
    match (&theirs, &ours) {
        (Ok(a), Ok(b)) if a == b => Ok(()),
        (Err(_), Err(_)) => Ok(()),
        _ => Err(format!(
            "nix and ogygia-nix-eval disagree\n--- nix:\n{}\n--- ours:\n{}",
            show(&theirs),
            show(&ours)
        )
        .into()),
    }
}

fn show(r: &Result<String, String>) -> String {
    match r {
        Ok(s) => s.clone(),
        Err(e) => format!("error: {}", e.trim_end()),
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read cases dir")
        .map(|e| e.expect("read cases dir entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            // Fixture directories hold files that cases import.
            if p.file_name().is_some_and(|n| n != "fixtures") {
                collect(&p, out);
            }
        } else if p.extension().is_some_and(|e| e == "nix") {
            out.push(p);
        }
    }
}

fn main() {
    let args = Arguments::from_args();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let mut files = Vec::new();
    collect(&root, &mut files);

    let version = check_nix_version();
    let trials = files
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&root)
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .into_owned();
            let version = version.clone();
            Trial::ignorable_test(name, move || {
                version?;
                match load_case(&path)? {
                    Some(case) => run_case(&case).map(|()| Completion::Completed),
                    None => Ok(Completion::ignored_with("needs OGYGIA_NIX_EVAL_NIXPKGS")),
                }
            })
        })
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
