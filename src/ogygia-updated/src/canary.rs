//! Persisted state for a canary: a trial that holds the host on a
//! chosen build instead of the tracked branch tip.
//!
//! The daemon exits and is restarted by systemd after every activation,
//! so this outlives the process and is the sole record of an in-flight
//! trial. The file is absent until the first canary runs; afterwards it
//! always describes the current or most-recent one, so `canary status`
//! can explain how the last trial ended.

use std::fs;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

/// Which build a canary holds the host on. One variant today; a
/// branch-following mode slots in later without touching the lifecycle
/// or the engine's floor, expiry, and finish handling, which are all
/// target-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum CanaryTarget {
    /// A commit resolved when the canary was issued. `branch` is kept for
    /// provenance and display only — the host does not track it.
    Pinned { branch: String, commit: String },
    // Following { branch: String },  // future: re-resolve the tip each cycle
}

/// Lifecycle of the current or most-recent canary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CanaryState {
    /// A trial in progress.
    Active {
        target: CanaryTarget,
        /// Absolute deadline; `None` for a canary with no timeout.
        expires_at: Option<DateTime<Utc>>,
        /// Kernel boot id when the trial began. A change means the host
        /// rebooted, losing the trial's ephemeral test activation.
        boot_id: String,
    },
    /// A trial that ended, retained so `canary status` can explain how.
    Finished {
        target: CanaryTarget,
        at: DateTime<Utc>,
        reason: FinishReason,
    },
}

/// Why a canary stopped holding the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// The pinned commit became an ancestor of the tracked tip.
    Merged,
    /// The deadline passed.
    Expired,
    /// A bare `ogygia update` superseded it.
    Cleared,
    /// The host rebooted, losing the trial's ephemeral test activation.
    Rebooted,
    /// The running system was replaced out of band — a manual switch, with
    /// no reboot.
    Overwritten,
}

impl FinishReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            FinishReason::Merged => "merged",
            FinishReason::Expired => "expired",
            FinishReason::Cleared => "cleared",
            FinishReason::Rebooted => "rebooted",
            FinishReason::Overwritten => "overwritten",
        }
    }
}

impl CanaryState {
    /// Load the record, or `None` if no canary has ever run.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading canary state {}", path.display()));
            }
        };
        let state = serde_json::from_str(&raw)
            .with_context(|| format!("parsing canary state {}", path.display()))?;
        Ok(Some(state))
    }

    /// A human-readable status line. `running` and `boot_floor` are the
    /// commits the host currently runs and will next boot; they are only
    /// rendered for an in-progress trial.
    pub fn describe(&self, now: DateTime<Utc>, running: &str, boot_floor: &str) -> String {
        match self {
            CanaryState::Active {
                target: CanaryTarget::Pinned { branch, commit },
                expires_at,
                ..
            } => {
                let expiry = match expires_at {
                    Some(at) => format!(
                        "expires {} (in {}h)",
                        at.format("%Y-%m-%d %H:%MZ"),
                        (*at - now).num_hours()
                    ),
                    None => "no timeout".to_string(),
                };
                format!(
                    "trialling {branch} @{commit}; running {running}, boot floor {boot_floor}; {expiry}"
                )
            }
            CanaryState::Finished {
                target: CanaryTarget::Pinned { branch, commit },
                at,
                reason,
            } => format!(
                "no active canary; last: {branch} @{commit} finished {} ({})",
                at.format("%Y-%m-%d %H:%MZ"),
                reason.label(),
            ),
        }
    }

    /// Persist the record, replacing any previous one atomically.
    pub fn store(&self, path: &Path) -> Result<()> {
        let payload = serde_json::to_vec_pretty(self).context("encoding canary state")?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, &payload)
            .with_context(|| format!("writing canary state {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("replacing canary state {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_no_canary() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("canary.json");
        assert!(CanaryState::load(&path).unwrap().is_none());
    }

    #[test]
    fn active_and_finished_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("canary.json");
        let target = CanaryTarget::Pinned {
            branch: "feature".to_string(),
            commit: "a".repeat(40),
        };

        let active = CanaryState::Active {
            target: target.clone(),
            expires_at: Some("2026-07-18T12:00:00Z".parse().unwrap()),
            boot_id: "boot-1".to_string(),
        };
        active.store(&path).unwrap();
        assert!(matches!(
            CanaryState::load(&path).unwrap(),
            Some(CanaryState::Active {
                expires_at: Some(_),
                ..
            })
        ));

        // Storing again replaces the record rather than appending.
        let finished = CanaryState::Finished {
            target,
            at: "2026-07-18T12:00:00Z".parse().unwrap(),
            reason: FinishReason::Merged,
        };
        finished.store(&path).unwrap();
        assert!(matches!(
            CanaryState::load(&path).unwrap(),
            Some(CanaryState::Finished {
                reason: FinishReason::Merged,
                ..
            })
        ));
    }

    #[test]
    fn describe_covers_active_and_finished() {
        let now: DateTime<Utc> = "2026-07-17T00:00:00Z".parse().unwrap();
        let target = CanaryTarget::Pinned {
            branch: "feature".to_string(),
            commit: "abc".to_string(),
        };

        let active = CanaryState::Active {
            target: target.clone(),
            expires_at: Some("2026-07-18T00:00:00Z".parse().unwrap()),
            boot_id: "boot-1".to_string(),
        };
        assert_eq!(
            active.describe(now, "def", "ghi"),
            "trialling feature @abc; running def, boot floor ghi; expires 2026-07-18 00:00Z (in 24h)"
        );

        let forever = CanaryState::Active {
            target: target.clone(),
            expires_at: None,
            boot_id: "boot-1".to_string(),
        };
        assert_eq!(
            forever.describe(now, "def", "ghi"),
            "trialling feature @abc; running def, boot floor ghi; no timeout"
        );

        let finished = CanaryState::Finished {
            target,
            at: now,
            reason: FinishReason::Merged,
        };
        assert_eq!(
            finished.describe(now, "def", "ghi"),
            "no active canary; last: feature @abc finished 2026-07-17 00:00Z (merged)"
        );
    }
}
