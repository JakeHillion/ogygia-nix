use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use chrono::TimeZone;
use chrono::Utc;
use git2::Oid;
use git2::Repository;
use tempfile::TempDir;

use crate::config::Config;
use crate::nixos::CommitInfo;

pub struct GitManager {
    temp_dir: Option<TempDir>,
    repo_path: Option<PathBuf>, // For test cases only
}

// Implement Send + Sync for GitManager since we manage our own repo access
unsafe impl Send for GitManager {}
unsafe impl Sync for GitManager {}

impl GitManager {
    pub async fn new(config: &Config) -> Result<Self> {
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        let repo_path = temp_dir.path();

        Repository::clone(config.git_repo_url(), repo_path)
            .context("Failed to clone nixos repository")?;

        Ok(Self {
            temp_dir: Some(temp_dir),
            repo_path: None,
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
        let mut remote = repo
            .find_remote("origin")
            .context("Failed to find origin remote")?;

        // Configure fetch options for comprehensive updates
        let mut fo = git2::FetchOptions::new();
        fo.download_tags(git2::AutotagOption::All);
        fo.prune(git2::FetchPrune::On);

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
