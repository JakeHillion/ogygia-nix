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
    // The runtime value, not env!: a nextest archive runs away from the tree
    // it was built in.
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("tests/cases");
    let mut files = Vec::new();
    collect(&root, &mut files);

    let version = check_nix_version();
    let mut trials: Vec<Trial> = files
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
    trials.extend(flake_trials(&manifest.join("tests/flakes"), &version));
    libtest_mimic::run(&args, trials).exit();
}

/// How a flake fixture is presented to both evaluators.
#[derive(Clone, Copy, Debug)]
enum FlakeMode {
    /// A plain directory, as a `path:` flake.
    Path,
    /// A Git repository with everything committed, plus an untracked file.
    Git,
    /// A Git repository with an uncommitted change to a tracked file.
    GitDirty,
    /// The `Git` repository, as a `path:` flake.
    PathInGit,
}

fn git() -> String {
    std::env::var("OGYGIA_NIX_EVAL_GIT")
        .ok()
        .or_else(|| option_env!("OGYGIA_GIT_BIN").map(str::to_owned))
        .unwrap_or_else(|| "git".to_owned())
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new(git())
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=equiv",
            "-c",
            "user.email=equiv@example.com",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            // Detached auto-maintenance would write to `.git` while a
            // `path:` fixture's tree is being hashed.
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .output()
        .map_err(|e| format!("running git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Copy a fixture into a fresh directory set up for `mode`.
fn stage_flake(fixture: &Path, mode: FlakeMode, dest: &Path) -> Result<(), String> {
    copy_tree(fixture, dest).map_err(|e| format!("copying fixture: {e}"))?;
    // Fixture files in the source tree may be read-only (the Nix store).
    let _ = Command::new("chmod").args(["-R", "u+w"]).arg(dest).status();
    std::fs::remove_file(dest.join("queries")).map_err(|e| e.to_string())?;
    match mode {
        FlakeMode::Path => {}
        FlakeMode::Git | FlakeMode::GitDirty | FlakeMode::PathInGit => {
            run_git(dest, &["init", "-q"])?;
            run_git(dest, &["add", "-A"])?;
            run_git(dest, &["commit", "-q", "-m", "fixture"])?;
            std::fs::write(dest.join("untracked.nix"), "untracked\n").map_err(|e| e.to_string())?;
            if matches!(mode, FlakeMode::GitDirty) {
                let flake_nix = dest.join("flake.nix");
                let mut text = std::fs::read_to_string(&flake_nix).map_err(|e| e.to_string())?;
                text.push_str("# uncommitted\n");
                std::fs::write(&flake_nix, text).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// A query line: an attribute path and an optional `--apply` function,
/// prefixed with `!` if evaluating it must fail.
struct Query {
    attr: String,
    apply: Option<String>,
    fails: bool,
}

fn parse_query(line: &str) -> Query {
    let (fails, line) = match line.trim().strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (attr, apply) = match line.split_once(" --apply ") {
        Some((attr, apply)) => (attr, Some(apply.trim().to_owned())),
        None => (line, None),
    };
    Query {
        attr: attr.trim().to_owned(),
        apply,
        fails,
    }
}

fn nix_eval_flake(
    flake_ref: &str,
    store: &Path,
    attr: &str,
    apply: Option<&str>,
) -> Result<String, String> {
    let nix = Path::new(&nix_instantiate()).with_file_name("nix");
    let mut cmd = Command::new(if nix.is_absolute() {
        nix.into_os_string()
    } else {
        "nix".into()
    });
    let s = scratch();
    cmd.env("HOME", s)
        .env("NIX_STATE_DIR", s.join("state"))
        .env("NIX_CONF_DIR", s.join("conf"))
        .env("NIX_LOG_DIR", s.join("log"))
        .env("XDG_CACHE_HOME", s.join("cache"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(["--option", "flake-registry", ""])
        .args(["--option", "use-registries", "false"])
        .arg("--store")
        .arg(format!("local?root={}", store.display()))
        .args(["eval", "--json", "--no-write-lock-file"])
        .arg(format!("{flake_ref}#{attr}"));
    if let Some(apply) = apply {
        cmd.args(["--apply", apply]);
    }
    let out = cmd.output().map_err(|e| format!("running nix: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn run_flake(fixture: &Path, mode: FlakeMode) -> Result<(), Failed> {
    let work = scratch().join(format!(
        "flake-{}-{mode:?}",
        fixture.file_name().unwrap().to_string_lossy()
    ));
    let _ = std::fs::remove_dir_all(&work);
    let dir = work.join("src");
    stage_flake(fixture, mode, &dir)?;
    let flake_ref = match mode {
        FlakeMode::Path | FlakeMode::PathInGit => format!("path:{}", dir.display()),
        FlakeMode::Git | FlakeMode::GitDirty => format!("git+file://{}", dir.display()),
    };
    let queries = std::fs::read_to_string(fixture.join("queries")).map_err(|e| e.to_string())?;
    let queries: Vec<Query> = queries
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(parse_query)
        .collect();

    let ours: Vec<Result<String, String>> = ogygia_nix_eval::with_flake(&flake_ref, |session| {
        queries
            .iter()
            .map(|q| {
                session
                    .eval_json(&q.attr, q.apply.as_deref())
                    .map(|v| v.to_string())
                    .map_err(|e| format!("{e:#}"))
            })
            .collect()
    })
    .map_err(|e| format!("opening flake: {e:#}"))?;

    let mut failures = Vec::new();
    for (q, ours) in queries.iter().zip(ours) {
        let theirs = nix_eval_flake(&flake_ref, &work.join("store"), &q.attr, q.apply.as_deref())
            .map(|s| normalise_json(&s));
        let agree = match (&theirs, &ours) {
            (Ok(a), Ok(b)) => !q.fails && *a == normalise_json(b),
            (Err(_), Err(_)) => q.fails,
            _ => false,
        };
        if !agree {
            failures.push(format!(
                "query {}{}{}\n--- nix:\n{}\n--- ours:\n{}",
                if q.fails { "(expected to fail) " } else { "" },
                q.attr,
                q.apply
                    .as_ref()
                    .map(|a| format!(" --apply {a}"))
                    .unwrap_or_default(),
                show(&theirs),
                show(&ours)
            ));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n").into())
    }
}

/// Re-serialise JSON so that formatting differences do not matter.
fn normalise_json(s: &str) -> String {
    serde_json::from_str::<serde_json::Value>(s)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| s.to_owned())
}

fn flake_trials(root: &Path, version: &Result<(), String>) -> Vec<Trial> {
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("read flakes dir")
        .map(|e| e.expect("read flakes dir entry").path())
        .filter(|p| p.join("queries").exists())
        .collect();
    fixtures.sort();
    let mut trials = Vec::new();
    for fixture in fixtures {
        for mode in [
            FlakeMode::Path,
            FlakeMode::PathInGit,
            FlakeMode::Git,
            FlakeMode::GitDirty,
        ] {
            let name = format!(
                "flakes/{}/{}",
                fixture.file_name().unwrap().to_string_lossy(),
                match mode {
                    FlakeMode::Path => "path",
                    FlakeMode::PathInGit => "path-in-git",
                    FlakeMode::Git => "git",
                    FlakeMode::GitDirty => "git-dirty",
                }
            );
            let version = version.clone();
            let fixture = fixture.clone();
            trials.push(Trial::test(name, move || {
                version?;
                run_flake(&fixture, mode)
            }));
        }
    }
    trials
}
