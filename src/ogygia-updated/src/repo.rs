use std::fs;

use anyhow::Context;
use anyhow::Result;
use git2::ErrorCode;
use git2::Oid;
use git2::Repository;
use git2::build::CheckoutBuilder;
use git2::build::RepoBuilder;
use tracing::info;

use crate::config::RepoConfig;

/// Refspec mirroring every remote head, so commits only reachable from
/// non-tracked branches (e.g. archived deployed commits) stay available
/// for ancestry checks.
const FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";

/// The daemon's private clone of the configuration repository.
pub struct Repo {
    inner: Repository,
}

impl Repo {
    pub fn open_or_clone(config: &RepoConfig) -> Result<Self> {
        let inner = match Repository::open(&config.path) {
            Ok(repository) => repository,
            Err(error) if error.code() == ErrorCode::NotFound => {
                info!(url = %config.url, path = %config.path.display(), "cloning repository");
                fs::create_dir_all(&config.path)
                    .with_context(|| format!("creating {}", config.path.display()))?;
                RepoBuilder::new()
                    .clone(&config.url, &config.path)
                    .with_context(|| format!("cloning {}", config.url))?
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("opening repository {}", config.path.display()));
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

#[cfg(test)]
pub mod testutil {
    use std::path::Path;

    use git2::Oid;
    use git2::Repository;
    use git2::Signature;

    /// Create a commit in `repository` updating `branch`, with `marker`
    /// written to a file to make each tree distinct.
    pub fn commit(repository: &Repository, branch: &str, parents: &[Oid], marker: &str) -> Oid {
        let signature = Signature::now("test", "test@example.com").unwrap();
        let blob = repository.blob(marker.as_bytes()).unwrap();
        let mut builder = repository.treebuilder(None).unwrap();
        builder.insert("marker", blob, 0o100644).unwrap();
        let tree = repository.find_tree(builder.write().unwrap()).unwrap();
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
    use super::testutil::init_origin;
    use super::*;
    use crate::config::RepoConfig;

    fn repo_config(origin: &Path, clone: &Path) -> RepoConfig {
        RepoConfig {
            url: origin.to_str().unwrap().to_string(),
            path: clone.to_path_buf(),
            branch: "main".to_string(),
        }
    }

    #[test]
    fn clones_fetches_and_tracks_tip() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);

        let config = repo_config(&origin_path, &clone_path);
        let repo = Repo::open_or_clone(&config).unwrap();
        repo.fetch().unwrap();
        assert_eq!(repo.tip("main").unwrap(), initial);

        let second = commit(&origin, "main", &[initial], "second");
        repo.fetch().unwrap();
        assert_eq!(repo.tip("main").unwrap(), second);

        // Reopening finds the existing clone rather than recloning.
        let repo = Repo::open_or_clone(&config).unwrap();
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

        let repo = Repo::open_or_clone(&repo_config(&origin_path, &clone_path)).unwrap();
        repo.fetch().unwrap();

        assert!(repo.is_ancestor(initial, second).unwrap());
        assert!(repo.is_ancestor(second, second).unwrap());
        assert!(!repo.is_ancestor(second, initial).unwrap());
        assert!(!repo.is_ancestor(side, second).unwrap());

        let unknown = Oid::from_str("0123456789012345678901234567890123456789").unwrap();
        assert!(!repo.is_ancestor(unknown, second).unwrap());
    }

    #[test]
    fn checkout_leaves_clean_tree_at_commit() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        let second = commit(&origin, "main", &[initial], "second");

        let repo = Repo::open_or_clone(&repo_config(&origin_path, &clone_path)).unwrap();
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
}
