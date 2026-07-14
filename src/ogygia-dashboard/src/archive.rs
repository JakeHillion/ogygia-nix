use std::collections::HashSet;
use std::sync::Arc;

use git2::Oid;

use crate::config::ArchiveConfig;
use crate::etcd::Etcd;
use crate::etcd::HostStates;
use crate::git::GitManager;

/// Spawn the background task keeping every commit referenced in etcd
/// reachable from the archive branch. Runs one pass immediately, then one per
/// host state change.
pub fn spawn(config: ArchiveConfig, git: Arc<GitManager>, etcd: Arc<Etcd>) {
    tokio::spawn(async move {
        let mut changes = etcd.subscribe();
        let mut archived: HashSet<Oid> = HashSet::new();

        loop {
            loop {
                let state = etcd.state().await;
                let candidates = collect_candidates(&state, &archived);
                if candidates.is_empty() {
                    break;
                }

                let git = git.clone();
                let config = config.clone();
                let result =
                    tokio::task::spawn_blocking(move || git.archive_commits(&config, &candidates))
                        .await
                        .expect("archive task panicked");

                match result {
                    Ok(done) => {
                        archived.extend(done);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Failed to archive deployed commits: {e:#}");
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    }
                }
            }

            if changes.changed().await.is_err() {
                return;
            }
        }
    });
}

/// Commits referenced by any host state that are not yet archived, paired
/// with the archive commit message describing who deployed them.
fn collect_candidates(state: &HostStates, archived: &HashSet<Oid>) -> Vec<(Oid, String)> {
    let mut oids: Vec<Oid> = state
        .host_states
        .values()
        .flat_map(|states| states.values().flatten().copied())
        .filter(|oid| !archived.contains(oid))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    oids.sort();

    oids.into_iter()
        .map(|oid| (oid, commit_message(oid, state)))
        .collect()
}

fn commit_message(oid: Oid, state: &HostStates) -> String {
    let mut hosts: Vec<String> = state
        .host_states
        .iter()
        .filter_map(|(host, states)| {
            let matching: Vec<String> = states
                .iter()
                .filter(|(_, deployed)| **deployed == Some(oid))
                .map(|(state, _)| state.as_ref().to_string())
                .collect();
            (!matching.is_empty()).then(|| format!("- {host} ({})", matching.join(", ")))
        })
        .collect();
    hosts.sort();

    format!(
        "Archive {} after deployment\n\nDeployed to:\n{}\n",
        &oid.to_string()[..12],
        hosts.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use enum_map::enum_map;

    use super::*;
    use crate::nixos::CommitState;

    #[test]
    fn test_collect_candidates() {
        let a = Oid::from_str("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let b = Oid::from_str("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();

        let mut state = HostStates::default();
        state.host_states.insert(
            "host1".to_string(),
            enum_map! {
                CommitState::Booted => Some(a),
                CommitState::Current => Some(b),
                CommitState::NextBoot => Some(b),
            },
        );
        state.host_states.insert(
            "host2".to_string(),
            enum_map! {
                CommitState::Booted => Some(b),
                CommitState::Current => Some(b),
                CommitState::NextBoot => None,
            },
        );

        let candidates = collect_candidates(&state, &HashSet::from([a]));
        let (oid, message) = match candidates.as_slice() {
            [candidate] => candidate,
            other => panic!("expected one candidate, got {other:?}"),
        };
        assert_eq!(*oid, b);
        assert_eq!(
            message,
            "Archive bbbbbbbbbbbb after deployment\n\n\
             Deployed to:\n\
             - host1 (current, nextboot)\n\
             - host2 (booted, current)\n"
        );
    }
}
