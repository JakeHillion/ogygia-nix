//! Test harness driving the real `jj` binary against a bare git origin.
//!
//! The change-id support in [`crate::repo`] keys off a header jj writes and
//! preserves across rewrites, so these tests build their history with jj
//! itself rather than imitating the commit objects it produces.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use git2::Oid;
use git2::Repository;
use git2::RepositoryInitOptions;

/// A bare git origin with a `main` branch, plus a jj working copy that
/// pushes to it. Every method panics on failure: a harness that cannot
/// drive jj has nothing to report beyond the command that broke.
pub struct Jj {
    origin: PathBuf,
    work: PathBuf,
}

impl Jj {
    /// Create the origin with an initial commit on `main`, and a jj working
    /// copy cloned from it.
    pub fn init(root: &Path) -> Self {
        let origin = root.join("origin.git");
        let work = root.join("work");
        Repository::init_opts(
            &origin,
            RepositoryInitOptions::new().bare(true).initial_head("main"),
        )
        .unwrap();
        run(
            root,
            &[
                "git",
                "clone",
                origin.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );

        let jj = Self { origin, work };
        jj.write("initial");
        jj.run(&["describe", "-m", "initial"]);
        jj.set_bookmark("main", "@");
        jj.push("main");
        jj
    }

    /// Path the daemon clones from.
    pub fn origin(&self) -> &Path {
        &self.origin
    }

    /// Start a commit on top of `parent` adding the file `file`, and leave
    /// the working copy on it.
    pub fn commit(&self, parent: &str, message: &str, file: &str) {
        self.run(&["new", parent]);
        self.write(file);
        self.run(&["describe", "-m", message]);
    }

    /// Reword `revision`, rewriting it under a new git hash while keeping
    /// its change-id — the `jj describe` half of the review cycle.
    pub fn describe(&self, revision: &str, message: &str) {
        self.run(&["describe", "-r", revision, "-m", message]);
    }

    /// Fold the file `file` into `revision`, rewriting it under a new git
    /// hash while keeping its change-id — the `jj squash` half of the review
    /// cycle.
    pub fn squash_into(&self, revision: &str, file: &str) {
        self.run(&["new", revision]);
        self.write(file);
        self.run(&["squash"]);
    }

    /// Move `revision` onto `destination`, rewriting it under a new git hash
    /// while keeping its change-id — how a single change lands ahead of the
    /// rest of the stack it was written on.
    pub fn rebase(&self, revision: &str, destination: &str) {
        self.run(&["rebase", "-r", revision, "-d", destination]);
    }

    /// Merge `revision` into `main` with a merge commit, leaving main's new
    /// tip unpushed. A merge commit keeps the branch's commits verbatim, so
    /// only their change-ids tie them to what the host was deployed on.
    pub fn merge_into_main(&self, revision: &str, message: &str) {
        self.run(&["new", "main", revision, "-m", message]);
        self.set_bookmark("main", "@");
    }

    /// Create `bookmark` or move it to `revision`. jj moves a bookmark with
    /// the commit it names when that commit is rewritten, so this is only
    /// needed to place one.
    pub fn set_bookmark(&self, bookmark: &str, revision: &str) {
        self.run(&["bookmark", "set", bookmark, "-r", revision]);
    }

    /// Push `bookmark` to the origin, returning the commit it now names
    /// there. jj force-pushes a bookmark whose remote value it knows, so
    /// this covers a rewrite as well as a first push.
    pub fn push(&self, bookmark: &str) -> Oid {
        self.run(&["git", "push", "--bookmark", bookmark]);
        self.tip(bookmark)
    }

    /// The commit `bookmark` names on the origin.
    pub fn tip(&self, bookmark: &str) -> Oid {
        Repository::open(&self.origin)
            .unwrap()
            .find_reference(&format!("refs/heads/{bookmark}"))
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .id()
    }

    /// Add a file to the working copy. jj snapshots it on the next command,
    /// so nothing needs adding. One file per change, so a change rebases
    /// onto a history without its neighbours rather than conflicting.
    fn write(&self, file: &str) {
        std::fs::write(self.work.join(file), file).unwrap();
    }

    fn run(&self, args: &[&str]) {
        run(&self.work, args);
    }
}

/// The `jj` binary. A Nix build bakes its store path in so the test archive
/// carries jj as a runtime dependency and needs nothing on the runner's
/// PATH; a plain `cargo test` falls back to PATH.
fn jj_bin() -> &'static str {
    option_env!("OGYGIA_JJ_BIN").unwrap_or("jj")
}

/// Run jj in `dir` under a fixed identity and no user configuration, so the
/// harness is unaffected by the developer's own jj settings.
fn run(dir: &Path, args: &[&str]) {
    let output = Command::new(jj_bin())
        .args(args)
        .current_dir(dir)
        .env("JJ_USER", "test")
        .env("JJ_EMAIL", "test@example.com")
        .env("JJ_CONFIG", "/dev/null")
        .output()
        .unwrap_or_else(|error| panic!("running `jj {}`: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "`jj {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
