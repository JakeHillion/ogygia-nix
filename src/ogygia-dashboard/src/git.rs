use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::TimeZone;
use chrono::Utc;
use git2::Oid;
use git2::Repository;
use tempfile::TempDir;

use crate::config::ArchiveConfig;
use crate::config::Config;
use crate::config::SshConfig;
use crate::nixos::CommitInfo;

pub struct GitManager {
    temp_dir: Option<TempDir>,
    repo_path: Option<PathBuf>, // For test cases only
    ssh: Option<SshConfig>,
}

// Implement Send + Sync for GitManager since we manage our own repo access
unsafe impl Send for GitManager {}
unsafe impl Sync for GitManager {}

impl GitManager {
    pub async fn new(config: &Config) -> Result<Self> {
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        let repo_path = temp_dir.path();

        let ssh = config.git.ssh.clone();
        {
            let url = ssh
                .as_ref()
                .map_or(config.git_repo_url(), |ssh| ssh.url.as_str());
            let mut builder = git2::build::RepoBuilder::new();
            if let Some(ssh) = &ssh {
                let mut fo = git2::FetchOptions::new();
                fo.remote_callbacks(ssh_callbacks(ssh));
                builder.fetch_options(fo);
            }
            builder
                .clone(url, repo_path)
                .context("Failed to clone nixos repository")?;
        }

        Ok(Self {
            temp_dir: Some(temp_dir),
            repo_path: None,
            ssh,
        })
    }

    fn open_repo(&self) -> Result<Repository> {
        let repo_path = if let Some(ref temp_dir) = self.temp_dir {
            temp_dir.path()
        } else if let Some(ref path) = self.repo_path {
            path
        } else {
            return Err(anyhow::anyhow!("No repository path available"));
        };
        Repository::open(repo_path).context("Failed to open repository")
    }

    pub async fn fetch_updates(&self) -> Result<()> {
        let repo = self.open_repo()?;
        fetch(&repo, self.ssh.as_ref())
    }

    /// Make every candidate commit reachable from the archive branch by
    /// pushing a chain of merge commits on top of it, so the commits survive
    /// their original branch being force-pushed. Returns the commits that are
    /// now reachable from the branch; candidates that could not be resolved
    /// locally are omitted and can be retried later.
    pub fn archive_commits(
        &self,
        config: &ArchiveConfig,
        candidates: &[(Oid, String)],
    ) -> Result<HashSet<Oid>> {
        let repo = self.open_repo()?;
        let tracking_ref = format!("refs/remotes/origin/{}", config.branch);
        let local_ref = format!("refs/heads/{}", config.branch);

        // Compare-and-swap loop: fetch the current archive head, build merge
        // commits on top of it, and push without force. A rejected push means
        // someone else advanced the branch; refetch and rebuild.
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 1..=MAX_ATTEMPTS {
            fetch(&repo, self.ssh.as_ref())?;

            let head = repo
                .find_reference(&tracking_ref)
                .ok()
                .and_then(|r| r.target());

            let mut done = HashSet::new();
            let mut pending = Vec::new();
            for (oid, message) in candidates {
                let reachable = head.is_some_and(|head| {
                    head == *oid || repo.graph_descendant_of(head, *oid).unwrap_or(false)
                });
                if reachable {
                    done.insert(*oid);
                } else {
                    pending.push((*oid, message));
                }
            }
            if pending.is_empty() {
                return Ok(done);
            }

            let mut head_commit = head.map(|oid| repo.find_commit(oid)).transpose()?;
            for (oid, message) in pending {
                let commit = match repo.find_commit(oid) {
                    Ok(commit) => commit,
                    Err(e) => {
                        tracing::warn!("Cannot archive {oid}: not found after fetch: {e}");
                        continue;
                    }
                };

                // Only reachability matters, so take the archived commit's
                // tree as-is instead of merging.
                let signature = git2::Signature::now("ogygia-dashboard", "dashboard@ogygia")?;
                let parents: Vec<&git2::Commit> =
                    head_commit.iter().chain(std::iter::once(&commit)).collect();
                let merged = repo.commit(
                    None,
                    &signature,
                    &signature,
                    message,
                    &commit.tree()?,
                    &parents,
                )?;
                head_commit = Some(repo.find_commit(merged)?);
                done.insert(oid);
            }

            let Some(new_head) = &head_commit else {
                return Ok(done);
            };
            repo.reference(&local_ref, new_head.id(), true, "archive deployed commits")
                .context("Failed to update local archive branch")?;

            match push_archive_branch(&repo, self.ssh.as_ref(), &config.branch, &local_ref) {
                Ok(()) => {
                    tracing::info!(
                        "Archived {} commit(s) to branch '{}'",
                        done.len(),
                        config.branch
                    );
                    return Ok(done);
                }
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::warn!("Archive push failed (attempt {attempt}), retrying: {e:#}");
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("archive push loop returns on final attempt")
    }

    /// Get commit metadata for the specified commit hashes.
    /// Also includes main branch tip and initial commit for context.
    pub fn get_commits_info(&self, commit_hashes: &HashSet<Oid>) -> Result<Vec<CommitInfo>> {
        let repo = self.open_repo()?;
        let mut commits = Vec::new();

        for &commit_oid in commit_hashes {
            match self.get_commit_info_from_oid(&repo, commit_oid, "") {
                Ok(commit_info) => commits.push(commit_info),
                Err(e) => {
                    tracing::warn!("Failed to get commit info for {commit_oid}: {e}");
                    commits.push(CommitInfo::Missing(commit_oid.to_string()));
                }
            }
        }

        // Include main tip and initial commit for context
        if let Ok(main_tip_oid) = self.get_main_tip() {
            if !commit_hashes.contains(&main_tip_oid) {
                match self.get_commit_info_from_oid(&repo, main_tip_oid, "") {
                    Ok(commit_info) => commits.push(commit_info),
                    Err(e) => {
                        tracing::warn!("Failed to get main tip commit info for {main_tip_oid}: {e}")
                    }
                }
            }

            // Add initial commit
            if let Ok(initial_oid) = self.get_initial_commit(&repo, main_tip_oid)
                && !commit_hashes.contains(&initial_oid)
            {
                match self.get_commit_info_from_oid(&repo, initial_oid, "") {
                    Ok(commit_info) => commits.push(commit_info),
                    Err(e) => {
                        tracing::warn!("Failed to get initial commit info for {initial_oid}: {e}")
                    }
                }
            }

            // Include merge-base for each commit with main branch
            let mut seen_commits = HashSet::new();
            for &commit_oid in commit_hashes {
                seen_commits.insert(commit_oid);
            }
            seen_commits.insert(main_tip_oid);
            if let Ok(initial_oid) = self.get_initial_commit(&repo, main_tip_oid) {
                seen_commits.insert(initial_oid);
            }

            for &commit_oid in &seen_commits.clone() {
                if let Ok(merge_base_oid) = repo.merge_base(commit_oid, main_tip_oid)
                    && !seen_commits.contains(&merge_base_oid)
                {
                    match self.get_commit_info_from_oid(&repo, merge_base_oid, "") {
                        Ok(commit_info) => {
                            commits.push(commit_info);
                            seen_commits.insert(merge_base_oid);
                        }
                        Err(e) => tracing::warn!(
                            "Failed to get merge-base commit info for {merge_base_oid}: {e}"
                        ),
                    }
                }
            }
        }

        // Sort by missing status first (missing commits at top), then by timestamp (newest first)
        commits.sort_by(|a, b| {
            match (a, b) {
                (CommitInfo::Missing(_), CommitInfo::Complete { .. }) => std::cmp::Ordering::Less,
                (CommitInfo::Complete { .. }, CommitInfo::Missing(_)) => {
                    std::cmp::Ordering::Greater
                }
                (CommitInfo::Missing(hash_a), CommitInfo::Missing(hash_b)) => hash_a.cmp(hash_b),
                (
                    CommitInfo::Complete {
                        timestamp: ts_a, ..
                    },
                    CommitInfo::Complete {
                        timestamp: ts_b, ..
                    },
                ) => {
                    ts_b.cmp(ts_a) // Newest first
                }
            }
        });

        Ok(commits)
    }

    pub fn get_main_tip(&self) -> Result<Oid> {
        let repo = self.open_repo()?;
        if let Ok(main_ref) = repo.find_reference("refs/remotes/origin/main") {
            main_ref
                .target()
                .ok_or_else(|| anyhow::anyhow!("No target for main ref"))
        } else if let Ok(main_ref) = repo.find_reference("refs/heads/main") {
            main_ref
                .target()
                .ok_or_else(|| anyhow::anyhow!("No target for main ref"))
        } else {
            Err(anyhow::anyhow!("No main branch found"))
        }
    }

    fn get_initial_commit(&self, repo: &Repository, main_oid: Oid) -> Result<Oid> {
        let mut walker = repo.revwalk()?;
        walker.push(main_oid)?;
        walker.set_sorting(git2::Sort::TIME | git2::Sort::REVERSE)?;

        walker
            .next()
            .ok_or_else(|| anyhow::anyhow!("No commits found"))?
            .map_err(|e| anyhow::anyhow!("Error walking commits: {}", e))
    }

    fn get_commit_info_from_oid(
        &self,
        repo: &Repository,
        commit_oid: Oid,
        hostname: &str,
    ) -> Result<CommitInfo> {
        let commit = repo
            .find_commit(commit_oid)
            .with_context(|| format!("object not found - no match for id ({commit_oid})"))?;

        let timestamp = Utc
            .timestamp_opt(commit.time().seconds(), 0)
            .single()
            .context("Invalid commit timestamp")?;

        // Determine branch - for now, assume main if not specified
        let branch = "main".to_string();

        Ok(CommitInfo::Complete {
            hash: commit_oid.to_string(),
            message: commit.message().unwrap_or("").to_string(),
            author: commit.author().name().unwrap_or("").to_string(),
            timestamp,
            branch,
            hosts_using: vec![hostname.to_string()],
        })
    }
}

fn ssh_callbacks(ssh: &SshConfig) -> git2::RemoteCallbacks<'_> {
    let mut callbacks = git2::RemoteCallbacks::new();
    callbacks.credentials(|_url, username_from_url, _allowed| {
        git2::Cred::ssh_key(
            username_from_url.unwrap_or("git"),
            None,
            &ssh.key_path,
            None,
        )
    });
    callbacks.certificate_check(|cert, host| check_host_key(cert, host, ssh.host_key.as_deref()));
    callbacks
}

fn fetch(repo: &Repository, ssh: Option<&SshConfig>) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("Failed to find origin remote")?;

    // Configure fetch options for comprehensive updates
    let mut fo = git2::FetchOptions::new();
    fo.download_tags(git2::AutotagOption::All);
    fo.prune(git2::FetchPrune::On);
    if let Some(ssh) = ssh {
        fo.remote_callbacks(ssh_callbacks(ssh));
    }

    remote
        .fetch(
            &[
                "+refs/heads/*:refs/remotes/origin/*",
                "+refs/tags/*:refs/tags/*",
            ],
            Some(&mut fo),
            None,
        )
        .context("Failed to fetch with full options")?;

    Ok(())
}

fn push_archive_branch(
    repo: &Repository,
    ssh: Option<&SshConfig>,
    branch: &str,
    local_ref: &str,
) -> Result<()> {
    let mut remote = repo
        .find_remote("origin")
        .context("Failed to find origin remote")?;

    let rejection = std::cell::RefCell::new(None);
    {
        let mut callbacks = ssh.map_or_else(git2::RemoteCallbacks::new, ssh_callbacks);
        // A rejected ref update (e.g. non-fast-forward) completes the push
        // successfully, so it has to be collected from the callback.
        callbacks.push_update_reference(|_refname, status| {
            if let Some(status) = status {
                *rejection.borrow_mut() = Some(status.to_string());
            }
            Ok(())
        });

        let mut options = git2::PushOptions::new();
        options.remote_callbacks(callbacks);
        remote
            .push(
                &[&format!("{local_ref}:refs/heads/{branch}")],
                Some(&mut options),
            )
            .context("Failed to push archive branch")?;
    }

    match rejection.into_inner() {
        Some(status) => Err(anyhow::anyhow!("Push of '{branch}' rejected: {status}")),
        None => Ok(()),
    }
}

fn check_host_key(
    cert: &git2::cert::Cert,
    host: &str,
    pinned: Option<&str>,
) -> Result<git2::CertificateCheckStatus, git2::Error> {
    let Some(host_key) = cert.as_hostkey().and_then(|k| k.hostkey()) else {
        return Ok(git2::CertificateCheckStatus::CertificatePassthrough);
    };

    match pinned {
        Some(pinned) => {
            // Accept any known_hosts style field ("ssh-ed25519 AAAA... comment").
            if pinned
                .split_whitespace()
                .any(|field| BASE64.decode(field).is_ok_and(|key| key == host_key))
            {
                Ok(git2::CertificateCheckStatus::CertificateOk)
            } else {
                Err(git2::Error::from_str(&format!(
                    "SSH host key for '{host}' does not match configured git.ssh.host_key"
                )))
            }
        }
        None => {
            tracing::warn!(
                "Accepting unverified SSH host key for '{host}': {}; set git.ssh.host_key to pin it",
                BASE64.encode(host_key)
            );
            Ok(git2::CertificateCheckStatus::CertificateOk)
        }
    }
}

#[cfg(test)]
mod tests {
    use git2::Signature;

    use super::*;

    const ARCHIVE_BRANCH: &str = "ogygia/deployed-commits-archive";

    fn create_commit(repo: &Repository, branch: &str, content: &str, parent: Option<Oid>) -> Oid {
        let blob = repo.blob(content.as_bytes()).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("file", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let signature = Signature::now("test", "test@example.com").unwrap();
        let parents: Vec<git2::Commit> = parent
            .map(|oid| repo.find_commit(oid).unwrap())
            .into_iter()
            .collect();
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
        let oid = repo
            .commit(None, &signature, &signature, content, &tree, &parent_refs)
            .unwrap();
        repo.reference(&format!("refs/heads/{branch}"), oid, true, "test")
            .unwrap();
        oid
    }

    fn push(repo: &Repository, refspec: &str) {
        let mut origin = repo.find_remote("origin").unwrap();
        origin.push(&[refspec], None).unwrap();
    }

    fn setup() -> (TempDir, Repository, Repository, GitManager, ArchiveConfig) {
        let dir = TempDir::new().unwrap();
        let remote_path = dir.path().join("remote.git");
        let remote = Repository::init_bare(&remote_path).unwrap();
        create_commit(&remote, "main", "initial", None);

        let clone_path = dir.path().join("clone");
        let clone = Repository::clone(remote_path.to_str().unwrap(), &clone_path).unwrap();
        let manager = GitManager {
            temp_dir: None,
            repo_path: Some(clone_path),
            ssh: None,
        };
        let config = ArchiveConfig {
            branch: ARCHIVE_BRANCH.to_string(),
        };
        (dir, remote, clone, manager, config)
    }

    fn archive_head(remote: &Repository) -> Oid {
        remote
            .find_reference(&format!("refs/heads/{ARCHIVE_BRANCH}"))
            .unwrap()
            .target()
            .unwrap()
    }

    #[test]
    fn test_archive_bootstrap_and_dedup() {
        let (_dir, remote, clone, manager, config) = setup();
        let c1 = create_commit(&clone, "pr", "c1", None);
        push(&clone, "refs/heads/pr:refs/heads/pr");

        let done = manager
            .archive_commits(&config, &[(c1, "Archive c1".to_string())])
            .unwrap();
        assert_eq!(done, HashSet::from([c1]));

        let head = archive_head(&remote);
        let head_commit = remote.find_commit(head).unwrap();
        assert_eq!(head_commit.message().unwrap(), "Archive c1");
        assert_eq!(head_commit.parent_ids().collect::<Vec<_>>(), vec![c1]);

        // Re-archiving an already reachable commit is a no-op.
        let done = manager
            .archive_commits(&config, &[(c1, "Archive c1".to_string())])
            .unwrap();
        assert_eq!(done, HashSet::from([c1]));
        assert_eq!(archive_head(&remote), head);
    }

    #[test]
    fn test_archive_survives_force_push() {
        let (_dir, remote, clone, manager, config) = setup();
        let c1 = create_commit(&clone, "pr", "c1", None);
        push(&clone, "refs/heads/pr:refs/heads/pr");
        manager
            .archive_commits(&config, &[(c1, "Archive c1".to_string())])
            .unwrap();
        let first_head = archive_head(&remote);

        // Force-push an unrelated commit over the PR branch.
        let c2 = create_commit(&clone, "pr", "c2", None);
        push(&clone, "+refs/heads/pr:refs/heads/pr");

        let done = manager
            .archive_commits(
                &config,
                &[
                    (c1, "Archive c1".to_string()),
                    (c2, "Archive c2".to_string()),
                ],
            )
            .unwrap();
        assert_eq!(done, HashSet::from([c1, c2]));

        let head_commit = remote.find_commit(archive_head(&remote)).unwrap();
        assert_eq!(head_commit.message().unwrap(), "Archive c2");
        assert_eq!(
            head_commit.parent_ids().collect::<Vec<_>>(),
            vec![first_head, c2]
        );
        assert_eq!(
            head_commit.tree_id(),
            remote.find_commit(c2).unwrap().tree_id()
        );
        // c1 is no longer reachable from pr but remains reachable via the archive.
        assert!(remote.graph_descendant_of(head_commit.id(), c1).unwrap());
    }

    #[test]
    fn test_archive_builds_on_latest_remote_head() {
        let (_dir, remote, clone, manager, config) = setup();
        let c1 = create_commit(&clone, "pr", "c1", None);
        push(&clone, "refs/heads/pr:refs/heads/pr");
        manager
            .archive_commits(&config, &[(c1, "Archive c1".to_string())])
            .unwrap();

        // Advance the remote archive branch behind the manager's back.
        let external = create_commit(&clone, "external", "external", Some(archive_head(&remote)));
        push(
            &clone,
            &format!("refs/heads/external:refs/heads/{ARCHIVE_BRANCH}"),
        );

        let c2 = create_commit(&clone, "pr", "c2", None);
        push(&clone, "+refs/heads/pr:refs/heads/pr");
        manager
            .archive_commits(&config, &[(c2, "Archive c2".to_string())])
            .unwrap();

        let head_commit = remote.find_commit(archive_head(&remote)).unwrap();
        assert_eq!(
            head_commit.parent_ids().collect::<Vec<_>>(),
            vec![external, c2]
        );
    }

    #[test]
    fn test_archive_skips_unresolvable_commits() {
        let (_dir, remote, _clone, manager, config) = setup();
        let missing = Oid::from_str("cccccccccccccccccccccccccccccccccccccccc").unwrap();

        let done = manager
            .archive_commits(&config, &[(missing, "Archive missing".to_string())])
            .unwrap();
        assert!(done.is_empty());
        assert!(
            remote
                .find_reference(&format!("refs/heads/{ARCHIVE_BRANCH}"))
                .is_err()
        );
    }
}
