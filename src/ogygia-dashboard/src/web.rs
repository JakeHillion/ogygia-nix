use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::response::IntoResponse;
use chrono::DateTime;
use chrono::Utc;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::etcd::Etcd;
use crate::etcd::HostStates;
use crate::git::GitManager;
use crate::nixos::CommitInfo;

#[derive(Clone)]
struct CachedCommits {
    version: usize,
    commits: Vec<CommitInfo>,
    expiry_time: DateTime<Utc>,
}

#[derive(Clone)]
struct CachedHtml {
    version: usize,
    pr_count: Option<u32>,
    html: String,
}

#[derive(Clone)]
struct CachedPrCount {
    count: Option<u32>,
    cached_at: DateTime<Utc>,
}

pub struct AppState {
    config: Config,
    git_manager: GitManager,
    etcd: Arc<Etcd>,
    commits_cache: Mutex<Arc<CachedCommits>>,
    html_cache: Mutex<Arc<CachedHtml>>,
    pr_count_cache: Mutex<Arc<CachedPrCount>>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self> {
        let git_manager = GitManager::new(&config).await?;
        git_manager.fetch_updates().await?;
        let etcd = Etcd::new(&config).await?;

        Ok(Self {
            config,
            git_manager,
            etcd,
            commits_cache: Mutex::new(Arc::new(CachedCommits {
                version: 0,
                commits: Vec::new(),
                expiry_time: DateTime::<Utc>::MAX_UTC,
            })),
            html_cache: Mutex::new(Arc::new(CachedHtml {
                version: 0,
                pr_count: None,
                html: String::new(),
            })),
            pr_count_cache: Mutex::new(Arc::new(CachedPrCount {
                count: None,
                cached_at: DateTime::from_timestamp(0, 0).unwrap(), // Unix epoch
            })),
        })
    }

    /// Get commit metadata for all commits referenced by etcd host states.
    /// Uses version-based caching to avoid redundant git operations.
    /// Also includes time-based expiry when commits have missing metadata.
    async fn get_commits(&self) -> Result<Arc<CachedCommits>> {
        let host_states = self.etcd.state().await;
        let now = Utc::now();

        let mut cache = self.commits_cache.lock().await;

        // Check if commits cache is up to date while holding the lock
        // Use >= in case cache was updated by another thread while we waited for the lock
        if cache.version >= host_states.version && now < cache.expiry_time {
            return Ok(cache.clone());
        }

        // Cache is stale or expired, we need to update it
        // If cache expired due to missing commits, run git fetch first
        if now >= cache.expiry_time && cache.version > 0 {
            self.git_manager.fetch_updates().await?;
        }

        // Collect all unique commit hashes from etcd state
        let mut commit_hashes = HashSet::new();
        for host_states in host_states.host_states.values() {
            for &oid in host_states.values().flatten() {
                commit_hashes.insert(oid);
            }
        }

        // Fetch commit info from git (this is the expensive operation)
        let commits = self.git_manager.get_commits_info(&commit_hashes)?;

        // Check if any commits are missing
        let has_missing_commits = commits.iter().any(|c| matches!(c, CommitInfo::Missing(_)));

        // Set expiry time: 1 minute from now if missing commits, 15 minutes otherwise to update the status of tracked branches
        let expiry_time = if has_missing_commits {
            now + chrono::Duration::minutes(1)
        } else {
            now + chrono::Duration::minutes(15)
        };

        // Update cache while still holding the lock
        let new_cache = Arc::new(CachedCommits {
            version: host_states.version,
            commits,
            expiry_time,
        });

        *cache = new_cache.clone();
        Ok(new_cache)
    }

    /// Get PR count with time-based caching (5 minute TTL).
    /// Independent of etcd version changes.
    async fn get_pr_count(&self) -> Option<u32> {
        const PR_COUNT_TTL_MINUTES: i64 = 5;

        let mut cache = self.pr_count_cache.lock().await;
        let now = Utc::now();

        // Check if cache is fresh (within TTL)
        if (now - cache.cached_at).num_minutes() < PR_COUNT_TTL_MINUTES {
            return cache.count;
        }

        // Cache is stale, fetch fresh PR count
        let pr_count = self.fetch_pr_count().await;

        *cache = Arc::new(CachedPrCount {
            count: pr_count,
            cached_at: now,
        });

        pr_count
    }

    /// Generate and cache HTML git graph visualization.
    /// Combines etcd host states with git commit metadata.
    async fn get_cached_html(&self) -> Result<String> {
        let host_states = self.etcd.state().await;
        let commits = self.get_commits().await?;
        let current_pr_count = self.get_pr_count().await;

        let mut cache = self.html_cache.lock().await;

        // Check if HTML cache is up to date while holding the lock
        // Cache is valid only if BOTH etcd version and PR count haven't changed
        let cache_valid =
            cache.version >= host_states.version && cache.pr_count == current_pr_count;

        if cache_valid {
            return Ok(cache.html.clone());
        }

        // Get the actual main tip OID from git
        let main_tip_oid = self.git_manager.get_main_tip().ok();

        // Cache is stale (either etcd state or PR count changed), regenerate HTML
        let html = generate_git_graph_html(
            &self.config,
            &host_states,
            &commits.commits,
            current_pr_count,
            main_tip_oid,
        );

        // Update cache while still holding the lock
        *cache = Arc::new(CachedHtml {
            version: host_states.version,
            pr_count: current_pr_count,
            html: html.clone(),
        });

        Ok(html)
    }

    async fn fetch_pr_count(&self) -> Option<u32> {
        let client = reqwest::Client::new();
        match client.get(self.config.pulls_api_url()).send().await {
            Ok(response) => {
                if let Some(total_count) = response.headers().get("x-total-count")
                    && let Ok(count_str) = total_count.to_str()
                    && let Ok(count) = count_str.parse::<u32>()
                    && count > 0
                {
                    return Some(count);
                }
                None
            }
            Err(_) => None,
        }
    }
}

pub async fn index(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Generate the HTML git graph content
    let git_graph_content = match state.get_cached_html().await {
        Ok(html) => html,
        Err(_) => "<p>Error generating git graph</p>".to_string(),
    };

    let title = &state.config.title;

    let html = format!(
        r#"
<!DOCTYPE html>
<html>
<head>
    <title>{title}</title>
    <style>
        body {{
            font-family: Arial, sans-serif;
            margin: 40px;
            background-color: #f5f5f5;
        }}
        .container {{
            max-width: 1200px;
            margin: 0 auto;
            background-color: white;
            padding: 20px;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
        }}
        h1 {{
            color: #333;
            text-align: center;
        }}
        .status-section {{
            margin: 20px 0;
            text-align: center;
        }}
        .svg-container {{
            margin: 20px 0;
            text-align: center;
        }}
        .section-header {{
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-bottom: 20px;
            border-bottom: 1px solid #e1e4e8;
            padding-bottom: 12px;
        }}
        .section-title {{
            margin: 0;
            color: #24292f;
            font-size: 20px;
        }}
        .section-links {{
            display: flex;
            gap: 10px;
        }}
        .section-links a {{
            color: #0969da;
            text-decoration: none;
            padding: 6px 12px;
            border: 1px solid #d1d5da;
            border-radius: 6px;
            font-size: 14px;
            background-color: #f6f8fa;
        }}
        .section-links a:hover {{
            background-color: #e1e4e8;
            border-color: #0969da;
        }}
    </style>
</head>
<body>
    <div class="container">
        <h1>{title}</h1>

        <div class="status-section">
            <div class="git-graph-container">
                {git_graph_content}
            </div>
        </div>
    </div>
</body>
</html>
    "#
    );

    Html(html)
}

async fn generate_html_git_graph(state: Arc<AppState>) -> Result<String, String> {
    // Generate cached HTML git graph (with PR count cached)
    state
        .get_cached_html()
        .await
        .map_err(|e| format!("Failed to generate HTML: {e}"))
}

pub async fn nixos_commits_html(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    // Generate HTML git graph
    match generate_html_git_graph(state).await {
        Ok(html_content) => Ok((
            StatusCode::OK,
            [("content-type", "text/html; charset=utf-8")],
            html_content,
        )),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to generate HTML: {e}"),
        )),
    }
}

fn generate_git_graph_html(
    config: &Config,
    host_states: &HostStates,
    commits: &[CommitInfo],
    pr_count: Option<u32>,
    main_tip_oid: Option<git2::Oid>,
) -> String {
    let mut html = String::new();

    // Analyze commit structure - all commits treated as main branch (linear timeline)
    let commit_graph = analyze_commit_structure(commits);

    // Add CSS styles
    html.push_str(
        r#"
<style>
.git-graph {
    font-family: 'Monaco', 'Menlo', 'Ubuntu Mono', monospace;
    font-size: 14px;
    line-height: 1.5;
    background: #fff;
    padding: 20px;
    border-radius: 8px;
    border: 1px solid #e1e4e8;
}

.commit-row {
    display: flex;
    align-items: center;
    margin-bottom: 8px;
    position: relative;
}

.commit-line {
    width: 24px;
    height: 24px;
    position: relative;
    margin-right: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
}

/* Gitea-style grid system */
.git-graph-line {
    position: absolute;
    width: 2px;
    height: 100%;
    top: 0;
    background: var(--line-color, #d1d5da);
}

.git-graph-line.main {
    --line-color: #22c55e;
    left: 11px;
}

.git-graph-line.branch {
    --line-color: #f97316;
    left: 19px;
}

.git-graph-line.dashed {
    background: repeating-linear-gradient(
        to bottom,
        var(--line-color) 0,
        var(--line-color) 3px,
        transparent 3px,
        transparent 6px
    );
}

/* Gitea-style connections */
.git-graph-connection {
    position: absolute;
    width: 10px;
    height: 2px;
    top: 11px;
    left: 11px;
    background: var(--line-color, #f97316);
}

.git-graph-connection.branch-out {
    --line-color: #f97316;
}

/* Angled connections like Gitea */
.git-graph-connection.angled::after {
    content: '';
    position: absolute;
    right: -2px;
    top: 0;
    width: 2px;
    height: 12px;
    background: var(--line-color, #f97316);
}

.git-graph-connection.angled-in::before {
    content: '';
    position: absolute;
    right: -2px;
    top: -10px;
    width: 2px;
    height: 12px;
    background: var(--line-color, #f97316);
}

.commit-bubble {
    width: 12px;
    height: 12px;
    background: #f6f8fa;
    border: 2px solid #d1d5da;
    border-radius: 50%;
    position: relative;
    z-index: 2;
    cursor: help;
}

.commit-bubble.main-branch {
    margin-right: 8px; /* Position on main line */
}

.commit-bubble.branch {
    margin-left: 8px; /* Position on branch line */
}


.commit-hash.missing {
    background: #fef2f2;
    border: 1px solid #fecaca;
}

.commit-info {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
    justify-content: space-between;
}

.commit-left {
    display: flex;
    align-items: center;
    flex: 1;
    min-width: 0;
}

.commit-right {
    display: flex;
    align-items: center;
    flex-shrink: 0;
}

.commit-hash {
    background: #f6f8fa;
    border: 1px solid #d1d5da;
    border-radius: 16px;
    padding: 4px 12px;
    margin-right: 12px;
    text-decoration: none;
    color: #0969da;
    font-weight: 500;
    font-size: 12px;
    white-space: nowrap;
}

.commit-hash:hover {
    background: #f3f4f6;
    border-color: #0969da;
}

.commit-author {
    color: #656d76;
    margin-right: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 120px;
}

.commit-date {
    color: #656d76;
    margin-right: 12px;
    font-size: 12px;
    white-space: nowrap;
    cursor: help;
}

.host-badges {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
}

.host-badge {
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    font-weight: 500;
    white-space: nowrap;
}

.host-badge.current-booted {
    background: #fed7aa;
    color: #ea580c;
    border: 1px solid #fdba74;
}

.host-badge.current-only {
    background: #fef3c7;
    color: #d97706;
    border: 1px solid #fcd34d;
}

.host-badge.booted-only {
    background: #e0f2fe;
    color: #0284c7;
    border: 1px solid #7dd3fc;
}

.host-badge.current-booted-nextboot {
    background: #f3e8ff;
    color: #7c3aed;
    border: 1px solid #c4b5fd;
}

.host-badge.current-nextboot {
    background: #dcfce7;
    color: #16a34a;
    border: 1px solid #86efac;
}

.host-badge.booted-nextboot {
    background: #cffafe;
    color: #0891b2;
    border: 1px solid #67e8f9;
}

.host-badge.nextboot-only {
    background: #fef2f2;
    color: #dc2626;
    border: 1px solid #fecaca;
}

.host-badge.main-tip {
    background: transparent;
    color: #656d76;
    border: 1px solid #d1d5da;
    font-weight: 500;
}

.host-badge.unknown {
    background: #f3f4f6;
    color: #6b7280;
    border: 1px solid #d1d5db;
}

.unknown-hosts {
    margin: 16px 0;
    padding: 12px;
    background: #fffbeb;
    border-radius: 6px;
    border: 1px solid #fcd34d;
}

.unknown-hosts-title {
    font-weight: 600;
    color: #92400e;
    margin-bottom: 8px;
}

.unknown-hosts-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
}

.unknown-host-item {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
}

.unknown-host-name {
    font-weight: 500;
    color: #1f2937;
}

.unknown-host-states {
    color: #6b7280;
}

.legend {
    margin-top: 20px;
    padding: 12px;
    background: #f6f8fa;
    border-radius: 6px;
    border: 1px solid #d1d5da;
}

.legend-title {
    font-weight: 600;
    margin-bottom: 8px;
    color: #24292f;
}

.legend-items {
    display: flex;
    flex-direction: column;
    gap: 8px;
}

.legend-item {
    display: flex;
    align-items: center;
    gap: 6px;
}

.legend-badge {
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 11px;
    font-weight: 500;
}

.legend-badge.current-booted {
    background: #fed7aa;
    color: #ea580c;
    border: 1px solid #fdba74;
}

.legend-badge.current-only {
    background: #fef3c7;
    color: #d97706;
    border: 1px solid #fcd34d;
}

.legend-badge.booted-only {
    background: #e0f2fe;
    color: #0284c7;
    border: 1px solid #7dd3fc;
}

.legend-badge.current-booted-nextboot {
    background: #f3e8ff;
    color: #7c3aed;
    border: 1px solid #c4b5fd;
}

.legend-badge.current-nextboot {
    background: #dcfce7;
    color: #16a34a;
    border: 1px solid #86efac;
}

.legend-badge.booted-nextboot {
    background: #cffafe;
    color: #0891b2;
    border: 1px solid #67e8f9;
}

.legend-badge.nextboot-only {
    background: #fef2f2;
    color: #dc2626;
    border: 1px solid #fecaca;
}
</style>
"#,
    );

    // Start git graph container
    html.push_str(r#"<div class="git-graph">"#);

    // Add section header inside the git graph
    let pr_count_text = match pr_count {
        Some(count) => format!(" ({count})"),
        None => String::new(),
    };

    html.push_str(&format!(
        r#"
        <div class="section-header">
            <h2 class="section-title">NixOS Host Status</h2>
            <div class="section-links">
                <a href="{}" target="_blank">Repository</a>
                <a href="{}" target="_blank">Pull Requests{pr_count_text}</a>
            </div>
        </div>
    "#,
        config.repo_web_url(),
        config.pulls_web_url()
    ));

    // Process commits using the analyzed graph structure
    for (index, node) in commit_graph.nodes.iter().enumerate() {
        let _next_node = commit_graph.nodes.get(index + 1);

        // Find the original commit info for host badges
        let commit = commits
            .iter()
            .find(|c| {
                let hash = match c {
                    CommitInfo::Missing(hash) => hash,
                    CommitInfo::Complete { hash, .. } => hash,
                };
                hash == &node.hash
            })
            .unwrap();

        // Collect hosts for this commit
        let mut host_badges = Vec::new();
        let hash = match commit {
            CommitInfo::Missing(hash) => hash,
            CommitInfo::Complete { hash, .. } => hash,
        };
        let Ok(commit_oid) = git2::Oid::from_str(hash) else {
            tracing::warn!("Skipping commit with invalid OID format: '{hash}'");
            continue;
        };

        // Add simple "main" badge for tip of main branch
        if main_tip_oid.map(|tip| tip == commit_oid).unwrap_or(false) {
            host_badges.push(r#"<span class="host-badge main-tip">main</span>"#.to_string());
        }

        for (hostname, states) in &host_states.host_states {
            use crate::nixos::CommitState;

            let is_current = states[CommitState::Current]
                .map(|oid| oid == commit_oid)
                .unwrap_or(false);
            let is_booted = states[CommitState::Booted]
                .map(|oid| oid == commit_oid)
                .unwrap_or(false);
            let is_nextboot = states[CommitState::NextBoot]
                .map(|oid| oid == commit_oid)
                .unwrap_or(false);

            if is_current || is_booted || is_nextboot {
                let clean_hostname = match &config.hostname_strip_suffix {
                    Some(suffix) => hostname.replace(suffix.as_str(), ""),
                    None => hostname.clone(),
                };
                let badge_class = match (is_current, is_booted, is_nextboot) {
                    (true, true, true) => "current-booted-nextboot",
                    (true, true, false) => "current-booted",
                    (true, false, true) => "current-nextboot",
                    (true, false, false) => "current-only",
                    (false, true, true) => "booted-nextboot",
                    (false, true, false) => "booted-only",
                    (false, false, true) => "nextboot-only",
                    (false, false, false) => unreachable!(),
                };
                host_badges.push(format!(
                    r#"<span class="host-badge {badge_class}">{clean_hostname}</span>"#
                ));
            }
        }

        // Determine line classes
        let mut line_classes = Vec::new();

        // Check if we need dashed line (skipped commits between main commits)
        if node.is_main_branch && index < commit_graph.nodes.len() - 1 {
            let next_main_distance = commit_graph
                .nodes
                .iter()
                .skip(index + 1)
                .position(|n| n.is_main_branch)
                .unwrap_or(0);
            if next_main_distance > 0 {
                line_classes.push("main-dashed");
            }
        }

        // Check if this is the first commit (no line above)
        if index == 0 {
            line_classes.push("no-line-above");
        }

        // Check if this is the last commit or last main commit (no line below)
        let has_main_commits_below = commit_graph
            .nodes
            .iter()
            .skip(index + 1)
            .any(|n| n.is_main_branch);
        if !has_main_commits_below {
            line_classes.push("no-line-below");
        }

        // Check if this commit has a branch coming off it
        let has_branch_child = commit_graph
            .nodes
            .iter()
            .any(|n| !n.is_main_branch && n.parents.contains(&node.hash));
        if has_branch_child {
            line_classes.push("has-branch");
        }

        // Format the commit date
        let timestamp = match commit {
            CommitInfo::Missing(_) => Utc::now(),
            CommitInfo::Complete { timestamp, .. } => *timestamp,
        };
        let (relative_date, absolute_date) = format_relative_date(timestamp);

        // Generate commit row with proper Gitea-style lines
        html.push_str(r#"<div class="commit-row">"#);

        // Add git graph lines and connections
        html.push_str(r#"<div class="commit-line">"#);

        // Main branch line (always present except for first/last)
        if index > 0 && has_main_commits_below {
            html.push_str(r#"<div class="git-graph-line main"></div>"#);
        }

        // Branch connections for non-main commits
        if !node.is_main_branch {
            html.push_str(r#"<div class="git-graph-connection branch-out angled"></div>"#);
            html.push_str(r#"<div class="git-graph-line branch"></div>"#);
        }

        // Commit bubble
        let mut bubble_classes = Vec::new();
        if node.is_main_branch {
            bubble_classes.push("main-branch");
        } else {
            bubble_classes.push("branch");
        }

        let message = match commit {
            CommitInfo::Missing(hash) => {
                format!("Missing commit ({})", &hash[..12.min(hash.len())])
            }
            CommitInfo::Complete { message, .. } => message.clone(),
        };

        html.push_str(&format!(
            r#"<div class="commit-bubble {}" title="{}"></div>"#,
            bubble_classes.join(" "),
            message.replace("\"", "&quot;")
        ));

        html.push_str(r#"</div>"#); // Close commit-line

        // Commit info
        let hash_class = if matches!(commit, CommitInfo::Missing(_)) {
            "commit-hash missing"
        } else {
            "commit-hash"
        };

        html.push_str(&format!(
            r#"<div class="commit-info">
                <div class="commit-left">
                    <a href="{}" class="{}" target="_blank" title="{}">
                        {}
                    </a>
                    <span class="commit-author">{}</span>
                    <div class="host-badges">
                        {}
                    </div>
                </div>
                <div class="commit-right">
                    <span class="commit-date" title="{}">{}</span>
                </div>
            </div>"#,
            config.commit_web_url(hash),
            hash_class,
            message.replace("\"", "&quot;"),
            commit.short_hash(),
            match commit {
                CommitInfo::Missing(_) => "Unknown",
                CommitInfo::Complete { author, .. } => author,
            },
            host_badges.join(""),
            absolute_date,
            relative_date
        ));

        html.push_str(r#"</div>"#); // Close commit-row
    }

    // Collect hosts with invalid (None) state values
    let unknown_hosts: Vec<_> = host_states
        .host_states
        .iter()
        .filter_map(|(hostname, states)| {
            let invalid_states: Vec<_> = [
                (
                    crate::nixos::CommitState::Current,
                    states[crate::nixos::CommitState::Current],
                ),
                (
                    crate::nixos::CommitState::Booted,
                    states[crate::nixos::CommitState::Booted],
                ),
                (
                    crate::nixos::CommitState::NextBoot,
                    states[crate::nixos::CommitState::NextBoot],
                ),
            ]
            .iter()
            .filter(|(_, oid)| oid.is_none())
            .map(|(state, _)| state.as_ref().to_string())
            .collect();

            if invalid_states.is_empty() {
                None
            } else {
                Some((hostname.clone(), invalid_states))
            }
        })
        .collect();

    if !unknown_hosts.is_empty() {
        html.push_str(r#"<div class="unknown-hosts">"#);
        html.push_str(
            r#"<div class="unknown-hosts-title">Hosts with invalid state data from etcd</div>"#,
        );
        html.push_str(r#"<div class="unknown-hosts-list">"#);

        for (hostname, invalid_states) in &unknown_hosts {
            let clean_hostname = match &config.hostname_strip_suffix {
                Some(suffix) => hostname.replace(suffix.as_str(), ""),
                None => hostname.to_string(),
            };
            html.push_str(&format!(
                r#"<div class="unknown-host-item"><span class="unknown-host-name">{}</span><span class="unknown-host-states">({})</span></div>"#,
                clean_hostname,
                invalid_states.join(", ")
            ));
        }

        html.push_str(r#"</div></div>"#);
    }

    // Add legend
    html.push_str(
        r#"
        <div class="legend">
            <div class="legend-items">
                <div class="legend-item">
                    <span class="legend-badge current-booted-nextboot">current & booted & nextboot</span>
                    <span>Host is running this commit, booted from it, and will boot it next</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge current-booted">current & booted</span>
                    <span>Host is running this commit and booted from it</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge current-nextboot">current & nextboot</span>
                    <span>Host is running this commit and will boot it next</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge current-only">current only</span>
                    <span>Host is running this commit but not booted from it</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge booted-nextboot">booted & nextboot</span>
                    <span>Host booted from this commit and will boot it next</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge booted-only">booted only</span>
                    <span>Host booted from this commit but not currently running it</span>
                </div>
                <div class="legend-item">
                    <span class="legend-badge nextboot-only">nextboot only</span>
                    <span>Host will boot this commit next</span>
                </div>
            </div>
        </div>
    "#,
    );

    // Close git graph container
    html.push_str("</div>");

    html
}

#[derive(Debug, Clone)]
struct CommitNode {
    hash: String,
    parents: Vec<String>,
    is_main_branch: bool,
}

#[derive(Debug)]
struct CommitGraph {
    nodes: Vec<CommitNode>,
}

/// Analyze commit structure into a linear graph.
/// TODO: Implement real graph analysis using git parent data for proper branch visualization.
/// For now, all commits are treated as main-branch (linear timeline).
fn analyze_commit_structure(commits: &[CommitInfo]) -> CommitGraph {
    let mut nodes = Vec::new();

    for (index, commit) in commits.iter().enumerate() {
        // All commits treated as main branch (linear timeline)
        let is_main_branch = true;

        // Connect to the next commit in chronological order
        let mut parents = Vec::new();
        if index < commits.len() - 1 {
            let next_commit = &commits[index + 1];
            let hash = match next_commit {
                CommitInfo::Missing(hash) => hash,
                CommitInfo::Complete { hash, .. } => hash,
            };
            parents.push(hash.clone());
        }

        let hash = match commit {
            CommitInfo::Missing(hash) => hash,
            CommitInfo::Complete { hash, .. } => hash,
        };

        nodes.push(CommitNode {
            hash: hash.clone(),
            parents,
            is_main_branch,
        });
    }

    CommitGraph { nodes }
}

fn format_relative_date(timestamp: DateTime<Utc>) -> (String, String) {
    let now = Utc::now();
    let duration = now.signed_duration_since(timestamp);

    let relative = if duration.num_days() >= 365 {
        let years = duration.num_days() / 365;
        if years == 1 {
            "1 year ago".to_string()
        } else {
            format!("{years} years ago")
        }
    } else if duration.num_days() >= 30 {
        let months = duration.num_days() / 30;
        if months == 1 {
            "1 month ago".to_string()
        } else {
            format!("{months} months ago")
        }
    } else if duration.num_days() >= 7 {
        let weeks = duration.num_days() / 7;
        if weeks == 1 {
            "1 week ago".to_string()
        } else {
            format!("{weeks} weeks ago")
        }
    } else if duration.num_days() >= 1 {
        let days = duration.num_days();
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{days} days ago")
        }
    } else if duration.num_hours() >= 1 {
        let hours = duration.num_hours();
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else if duration.num_minutes() >= 1 {
        let minutes = duration.num_minutes();
        if minutes == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{minutes} minutes ago")
        }
    } else {
        "just now".to_string()
    };

    let absolute = timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string();

    (relative, absolute)
}

#[cfg(test)]
mod tests {

    // Note: Tests are simplified for the new architecture
    // Full integration tests would require actual etcd setup

    // Tests for basic functionality would go here
    // Full integration tests require real etcd and git setup
}
