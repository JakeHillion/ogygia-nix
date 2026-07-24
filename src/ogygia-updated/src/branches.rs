//! Branch names available to trial with `ogygia update canary`.

use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use git2::BranchType;
use git2::Direction;
use git2::Remote;
use git2::Repository;

use crate::config::Config;

/// Bounds on the remote query. Completion is interactive, so an unreachable
/// remote must fail fast enough to fall back rather than stall the terminal.
const CONNECT_TIMEOUT_MS: i32 = 750;
const OPERATION_TIMEOUT_MS: i32 = 1_500;

/// Branch names to offer for a canary, sorted.
///
/// Prefers the remote, which is authoritative for a branch pushed moments
/// ago, and falls back to the daemon's clone when the remote is slow or
/// unreachable. Yields an empty list rather than an error because the caller
/// is shell completion, where a diagnostic would corrupt the candidates.
pub fn list(config: &Config) -> Vec<String> {
    list_remote(&config.repo.url)
        .or_else(|_| list_cloned(&config.repo_path()))
        .unwrap_or_default()
}

/// Branch names on the remote, without cloning it.
pub fn list_remote(url: &str) -> Result<Vec<String>> {
    // SAFETY: these set process-global libgit2 options. Completion is
    // single-threaded and does no other libgit2 work concurrently.
    unsafe {
        git2::opts::set_server_connect_timeout_in_milliseconds(CONNECT_TIMEOUT_MS)
            .context("setting connect timeout")?;
        git2::opts::set_server_timeout_in_milliseconds(OPERATION_TIMEOUT_MS)
            .context("setting operation timeout")?;
    }

    let mut remote = Remote::create_detached(url).with_context(|| format!("resolving {url}"))?;
    remote
        .connect(Direction::Fetch)
        .with_context(|| format!("connecting to {url}"))?;

    let mut branches: Vec<String> = remote
        .list()
        .with_context(|| format!("listing refs on {url}"))?
        .iter()
        .filter_map(|head| head.name().strip_prefix("refs/heads/"))
        .map(str::to_owned)
        .collect();
    remote.disconnect().context("disconnecting")?;

    branches.sort();
    Ok(branches)
}

/// Branch names the daemon's clone last mirrored from the remote.
pub fn list_cloned(path: &Path) -> Result<Vec<String>> {
    let repository =
        Repository::open(path).with_context(|| format!("opening repository {}", path.display()))?;

    let mut branches = Vec::new();
    for branch in repository.branches(Some(BranchType::Remote))? {
        let (branch, _) = branch?;
        // `origin/HEAD` is a symbolic ref, not a branch anyone can trial.
        if let Some(name) = branch.name()?.and_then(|name| name.strip_prefix("origin/"))
            && name != "HEAD"
        {
            branches.push(name.to_owned());
        }
    }

    branches.sort();
    Ok(branches)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::repo::Repo;
    use crate::repo::testutil::commit;
    use crate::repo::testutil::init_origin;

    #[test]
    fn lists_remote_branches_without_cloning() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let (origin, initial) = init_origin(&origin_path);
        commit(&origin, "jh/canary", &[initial], "canary");

        let branches = list_remote(origin_path.to_str().unwrap()).unwrap();

        assert_eq!(branches, ["jh/canary", "main"]);
        // Nothing was written next to the caller's cwd or the origin.
        assert!(!origin_path.join("clone").exists());
    }

    #[test]
    fn remote_listing_fails_for_an_unreachable_url() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does-not-exist");

        assert!(list_remote(missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn lists_cloned_branches_without_the_origin_head_ref() {
        let dir = TempDir::new().unwrap();
        let origin_path = dir.path().join("origin");
        let clone_path = dir.path().join("clone");
        let (origin, initial) = init_origin(&origin_path);
        commit(&origin, "jh/canary", &[initial], "canary");

        let repo = Repo::open_or_clone(&clone_path, origin_path.to_str().unwrap()).unwrap();
        repo.fetch().unwrap();

        let branches = list_cloned(&clone_path).unwrap();

        assert_eq!(branches, ["jh/canary", "main"]);
    }

    #[test]
    fn cloned_listing_fails_when_the_daemon_has_no_clone_yet() {
        let dir = TempDir::new().unwrap();

        assert!(list_cloned(&dir.path().join("absent")).is_err());
    }
}
