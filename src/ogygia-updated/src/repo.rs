use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use git2::ErrorCode;
use git2::Oid;
use git2::Repository;
use git2::build::CheckoutBuilder;
use git2::build::RepoBuilder;
use tracing::info;

/// Refspec mirroring every remote head, so commits only reachable from
/// non-tracked branches (e.g. archived deployed commits) stay available
/// for ancestry checks.
const FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";

/// Maximum commits scanned on either side of the divergence when deciding
/// whether an off-branch revision's changes have landed.
const MAX_LANDED_SCAN: usize = 1000;

/// The daemon's private clone of the configuration repository.
pub struct Repo {
    inner: Repository,
}

impl Repo {
    pub fn open_or_clone(path: &Path, url: &str) -> Result<Self> {
        let inner = match Repository::open(path) {
            Ok(repository) => repository,
            Err(error) if error.code() == ErrorCode::NotFound => {
                info!(url, path = %path.display(), "cloning repository");
                fs::create_dir_all(path).with_context(|| format!("creating {}", path.display()))?;
                RepoBuilder::new()
                    .clone(url, path)
                    .with_context(|| format!("cloning {url}"))?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("opening repository {}", path.display()));
            }
        };
        Ok(Self { inner })
    }

    pub fn fetch(&self) -> Result<()> {
        let mut remote = self.inner.find_remote("origin").context("finding origin")?;
        remote
            .fetch(&[FETCH_REFSPEC], None, None)
            .context("fetching from origin")?;
        Ok(())
    }

    /// Tip commit of `branch` on the remote.
    pub fn tip(&self, branch: &str) -> Result<Oid> {
        let reference = format!("refs/remotes/origin/{branch}");
        let commit = self
            .inner
            .find_reference(&reference)
            .with_context(|| format!("finding {reference}"))?
            .peel_to_commit()
            .with_context(|| format!("resolving {reference} to a commit"))?;
        Ok(commit.id())
    }

    /// Whether `ancestor` is `descendant` or one of its ancestors. Commits
    /// unknown to the repository are not ancestors of anything.
    pub fn is_ancestor(&self, ancestor: Oid, descendant: Oid) -> Result<bool> {
        if ancestor == descendant {
            return Ok(self.inner.find_commit(ancestor).is_ok());
        }
        if self.inner.find_commit(ancestor).is_err() {
            return Ok(false);
        }
        self.inner
            .graph_descendant_of(descendant, ancestor)
            .context("walking commit graph")
    }

    /// Newest commit that is an ancestor of both `a` and `b` — the point
    /// their histories diverge. Errors if they share no history.
    pub fn merge_base(&self, a: Oid, b: Oid) -> Result<Oid> {
        self.inner
            .merge_base(a, b)
            .with_context(|| format!("finding merge base of {a} and {b}"))
    }

    /// Whether every commit on `revision` but not yet on `tip` has a
    /// counterpart on `tip` sharing its change-id. This is the change-id
    /// generalisation of ancestry: jj rewrites commits as they land but
    /// preserves the change-id, so a deployed revision counts as merged
    /// once each of its changes reappears on the branch under a new git
    /// hash — even a whole stack landed one commit at a time.
    ///
    /// A single un-landed change is enough to fail: a deployed head whose
    /// change-id is on the branch has *not* merged if an ancestor of it
    /// hasn't. A commit without a change-id (plain git) cannot be shown to
    /// have landed, so its presence on `revision` fails too, as does a
    /// `revision` the clone has never fetched.
    pub fn changes_landed(&self, revision: Oid, tip: Oid) -> Result<bool> {
        if self.inner.find_commit(revision).is_err() {
            return Ok(false);
        }

        // Change-ids carried by the branch, bounded to the history since
        // the two diverged so an unrelated older change can't match.
        let base = self.inner.merge_base(revision, tip).ok();
        let mut branch_walk = self.inner.revwalk().context("starting branch walk")?;
        branch_walk.push(tip).context("walking from branch tip")?;
        if let Some(base) = base {
            branch_walk.hide(base).context("hiding common history")?;
        }
        let mut landed = HashSet::new();
        for oid in branch_walk.take(MAX_LANDED_SCAN) {
            let oid = oid.context("walking branch history")?;
            let commit = self
                .inner
                .find_commit(oid)
                .with_context(|| format!("finding commit {oid}"))?;
            if let Some(id) = change_id(&commit) {
                landed.insert(id);
            }
        }

        // Every change unique to the deployed revision must be among them.
        let mut deployed_walk = self.inner.revwalk().context("starting deployed walk")?;
        deployed_walk
            .push(revision)
            .context("walking from the deployed revision")?;
        deployed_walk
            .hide(tip)
            .context("hiding commits already on the branch")?;
        for (scanned, oid) in deployed_walk.enumerate() {
            if scanned >= MAX_LANDED_SCAN {
                return Ok(false);
            }
            let oid = oid.context("walking the deployed revision")?;
            let commit = self
                .inner
                .find_commit(oid)
                .with_context(|| format!("finding commit {oid}"))?;
            match change_id(&commit) {
                Some(id) if landed.contains(&id) => {}
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    /// Force the working tree onto `commit` with a detached HEAD, leaving
    /// it clean so flake evaluation sees the commit as the revision.
    pub fn checkout(&self, commit: Oid) -> Result<()> {
        let object = self
            .inner
            .find_object(commit, None)
            .with_context(|| format!("finding commit {commit}"))?;
        self.inner
            .checkout_tree(
                &object,
                Some(CheckoutBuilder::new().force().remove_untracked(true)),
            )
            .with_context(|| format!("checking out {commit}"))?;
        self.inner
            .set_head_detached(commit)
            .with_context(|| format!("detaching HEAD at {commit}"))?;
        Ok(())
    }
}

/// The change-id header jj preserves when rewriting a commit, e.g. when
/// landing it onto the tracked branch.
fn change_id(commit: &git2::Commit) -> Option<Vec<u8>> {
    commit
        .header_field_bytes("change-id")
        .ok()
        .map(|buf| buf.to_vec())
}

#[cfg(test)]
pub mod testutil {
    use std::path::Path;

    use git2::Oid;
    use git2::Repository;
    use git2::Signature;

    fn tree_with_marker(repository: &Repository, marker: &str) -> Oid {
        let blob = repository.blob(marker.as_bytes()).unwrap();
        let mut builder = repository.treebuilder(None).unwrap();
        builder.insert("marker", blob, 0o100644).unwrap();
        builder.write().unwrap()
    }

    /// Create a commit in `repository` updating `branch`, with `marker`
    /// written to a file to make each tree distinct.
    pub fn commit(repository: &Repository, branch: &str, parents: &[Oid], marker: &str) -> Oid {
        let signature = Signature::now("test", "test@example.com").unwrap();
        let tree = repository
            .find_tree(tree_with_marker(repository, marker))
            .unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| repository.find_commit(*oid).unwrap())
            .collect();
        let parent_refs: Vec<_> = parents.iter().collect();
        repository
            .commit(
                Some(&format!("refs/heads/{branch}")),
                &signature,
                &signature,
                marker,
                &tree,
                &parent_refs,
            )
            .unwrap()
    }

    /// Like `commit`, but with a jj-style change-id header embedded in
    /// the commit object.
    pub fn commit_with_change_id(
        repository: &Repository,
        branch: &str,
        parents: &[Oid],
        marker: &str,
        change_id: &str,
    ) -> Oid {
        let signature = Signature::now("test", "test@example.com").unwrap();
        let tree = repository
            .find_tree(tree_with_marker(repository, marker))
            .unwrap();
        let parents: Vec<_> = parents
            .iter()
            .map(|oid| repository.find_commit(*oid).unwrap())
            .collect();
        let parent_refs: Vec<_> = parents.iter().collect();
        let buffer = repository
            .commit_create_buffer(&signature, &signature, marker, &tree, &parent_refs)
            .unwrap();
        let text = std::str::from_utf8(&buffer).unwrap();
        let (headers, message) = text.split_once("\n\n").unwrap();
        let raw = format!("{headers}\nchange-id {change_id}\n\n{message}");
        let oid = repository
            .odb()
            .unwrap()
            .write(git2::ObjectType::Commit, raw.as_bytes())
            .unwrap();
        repository
            .reference(&format!("refs/heads/{branch}"), oid, true, "test")
            .unwrap();
        oid
    }

    /// Initialize an origin repository with an initial commit on main.
    pub fn init_origin(path: &Path) -> (Repository, Oid) {
        let repository = Repository::init(path).unwrap();
        repository.set_head("refs/heads/main").unwrap();
        let initial = commit(&repository, "main", &[], "initial");
        (repository, initial)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::testutil::commit;
    use super::testutil::commit_with_change_id;
    use super::testutil::init_origin;
    use super::*;

    fn open(origin: &Path, clone: &Path) -> Repo {
        Repo::open_or_clone(clone, origin.to_str().unwrap()).unwrap()
    }

    #[test]
    fn clones_fetches_and_tracks_tip() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();
        assert_eq!(repo.tip("main").unwrap(), initial);

        let second = commit(&origin, "main", &[initial], "second");
        repo.fetch().unwrap();
        assert_eq!(repo.tip("main").unwrap(), second);

        // Reopening finds the existing clone rather than recloning.
        let repo = open(&origin_path, &clone_path);
        assert_eq!(repo.tip("main").unwrap(), second);
    }

    #[test]
    fn ancestry_covers_unknown_and_side_commits() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let second = commit(&origin, "main", &[initial], "second");
        let side = commit(&origin, "side", &[initial], "side");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        assert!(repo.is_ancestor(initial, second).unwrap());
        assert!(repo.is_ancestor(second, second).unwrap());
        assert!(!repo.is_ancestor(second, initial).unwrap());
        assert!(!repo.is_ancestor(side, second).unwrap());

        let unknown = Oid::from_str("0123456789012345678901234567890123456789").unwrap();
        assert!(!repo.is_ancestor(unknown, second).unwrap());
    }

    #[test]
    fn merge_base_finds_the_branch_point() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let main_tip = commit(&origin, "main", &[initial], "second");
        let side = commit(&origin, "side", &[initial], "side");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        // The side branch diverged from main at the initial commit.
        assert_eq!(repo.merge_base(side, main_tip).unwrap(), initial);
        // A commit already on the branch is its own merge base with the tip.
        assert_eq!(repo.merge_base(initial, main_tip).unwrap(), initial);
    }

    #[test]
    fn checkout_leaves_clean_tree_at_commit() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let second = commit(&origin, "main", &[initial], "second");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();
        repo.checkout(second).unwrap();

        assert_eq!(
            std::fs::read_to_string(clone_path.join("marker")).unwrap(),
            "second"
        );
        let statuses = repo.inner.statuses(None).unwrap();
        assert!(statuses.is_empty());
        assert_eq!(repo.inner.head().unwrap().target(), Some(second));
    }

    #[test]
    fn single_landed_change_counts_as_merged() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let deployed =
            commit_with_change_id(&origin, "deploy", &[initial], "deployed", "kxyzkxyzkxyz");
        let landed = commit_with_change_id(&origin, "main", &[initial], "landed", "kxyzkxyzkxyz");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        // The rewritten commit has a new git hash, so it is not an ancestor,
        // but its change-id landed on the branch.
        assert!(!repo.is_ancestor(deployed, landed).unwrap());
        assert!(repo.changes_landed(deployed, landed).unwrap());
    }

    #[test]
    fn a_whole_landed_stack_counts_as_merged() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);

        // A two-commit stack deployed from a branch...
        let lower = commit_with_change_id(&origin, "deploy", &[initial], "lower", "aaaaaaaaaaaa");
        let deployed = commit_with_change_id(&origin, "deploy", &[lower], "upper", "bbbbbbbbbbbb");
        // ...both rewritten onto main, keeping their change-ids.
        let relanded = commit_with_change_id(&origin, "main", &[initial], "lower", "aaaaaaaaaaaa");
        let tip = commit_with_change_id(&origin, "main", &[relanded], "upper", "bbbbbbbbbbbb");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        assert!(repo.changes_landed(deployed, tip).unwrap());
    }

    #[test]
    fn a_stack_with_an_outstanding_change_has_not_merged() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);

        // The host runs a two-commit stack...
        let lower = commit_with_change_id(&origin, "deploy", &[initial], "lower", "aaaaaaaaaaaa");
        let deployed = commit_with_change_id(&origin, "deploy", &[lower], "upper", "bbbbbbbbbbbb");
        // ...but only the upper change has landed; the lower one has not.
        let tip = commit_with_change_id(&origin, "main", &[initial], "upper", "bbbbbbbbbbbb");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        // The deployed head's change-id is on the branch, but its
        // still-outstanding ancestor means the stack has not merged.
        assert!(!repo.changes_landed(deployed, tip).unwrap());
    }

    #[test]
    fn differing_or_missing_change_ids_are_not_landed() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let deployed =
            commit_with_change_id(&origin, "deploy", &[initial], "deployed", "kxyzkxyzkxyz");
        let plain = commit(&origin, "side", &[initial], "plain");
        let landed = commit_with_change_id(&origin, "main", &[initial], "landed", "koooooooooo");

        let repo = open(&origin_path, &clone_path);
        repo.fetch().unwrap();

        // A change-id is present on both sides but differs.
        assert!(!repo.changes_landed(deployed, landed).unwrap());
        // The deployed commit has no change-id header at all.
        assert!(!repo.changes_landed(plain, landed).unwrap());
        // The deployed commit is unknown to the repository.
        let unknown = Oid::from_str("0123456789012345678901234567890123456789").unwrap();
        assert!(!repo.changes_landed(unknown, landed).unwrap());
    }
}
