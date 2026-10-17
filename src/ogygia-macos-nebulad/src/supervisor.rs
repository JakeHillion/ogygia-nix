//! Keeping one Nebula process running on the current config.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncRead;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::protocol::Nebula;
use crate::protocol::Status;

/// The first restart delay after Nebula exits, doubled per failure.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A run this long counts as healthy and resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(60);
/// How long Nebula gets to tear its interface down before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

pub struct Supervisor {
    pub nebula: PathBuf,
    pub config: PathBuf,
    pub status: Arc<Mutex<Status>>,
}

impl Supervisor {
    /// Run Nebula on the config file whenever it exists, restarting it when
    /// `changes` ticks and after it exits, until `shutdown` is set.
    ///
    /// Any change restarts Nebula rather than asking it to reload: changes
    /// are rare (a cert renewal is monthly), and a restart is the one path
    /// that also handles a changed address.
    pub async fn run(self, mut changes: watch::Receiver<u64>, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = MIN_BACKOFF;
        loop {
            if *shutdown.borrow() {
                return;
            }
            changes.borrow_and_update();

            if !self.config.exists() {
                self.set(Nebula::Stopped);
                tokio::select! {
                    _ = changes.changed() => continue,
                    _ = shutdown.changed() => return,
                }
            }

            let (mut child, last_line) = match self.spawn() {
                Ok(spawned) => spawned,
                Err(e) => {
                    self.set(Nebula::Exited {
                        reason: format!("could not start nebula: {e}"),
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(MAX_BACKOFF) => continue,
                        _ = changes.changed() => continue,
                        _ = shutdown.changed() => return,
                    }
                }
            };
            let started = Instant::now();
            self.set(Nebula::Running { since: Utc::now() });

            tokio::select! {
                exit = child.wait() => {
                    let last = last_line.lock().unwrap().clone();
                    let reason = match (exit, last) {
                        (_, Some(line)) => line,
                        (Ok(status), None) => format!("nebula exited: {status}"),
                        (Err(e), None) => format!("could not wait for nebula: {e}"),
                    };
                    tracing::warn!(%reason, "nebula exited");
                    self.set(Nebula::Exited { reason });

                    if started.elapsed() >= HEALTHY_RUN {
                        backoff = MIN_BACKOFF;
                    }
                    let delay = backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        // A new config may fix whatever made it exit.
                        _ = changes.changed() => backoff = MIN_BACKOFF,
                        _ = shutdown.changed() => return,
                    }
                }
                _ = changes.changed() => {
                    tracing::info!("config changed; restarting nebula");
                    stop(&mut child).await;
                    backoff = MIN_BACKOFF;
                }
                _ = shutdown.changed() => {
                    stop(&mut child).await;
                    self.set(Nebula::Stopped);
                    return;
                }
            }
        }
    }

    fn set(&self, nebula: Nebula) {
        self.status.lock().unwrap().nebula = nebula;
    }

    /// Start Nebula, forwarding its log to ours and remembering its last line
    /// to explain an exit.
    fn spawn(&self) -> std::io::Result<(Child, Arc<Mutex<Option<String>>>)> {
        let mut child = Command::new(&self.nebula)
            .arg("-config")
            .arg(&self.config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        tracing::info!(pid = child.id(), "started nebula");

        let last_line = Arc::new(Mutex::new(None));
        if let Some(out) = child.stdout.take() {
            tokio::spawn(forward(out, last_line.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(forward(err, last_line.clone()));
        }
        Ok((child, last_line))
    }
}

async fn forward(stream: impl AsyncRead + Unpin, last_line: Arc<Mutex<Option<String>>>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        eprintln!("nebula: {line}");
        *last_line.lock().unwrap() = Some(line);
    }
}

/// Ask Nebula to exit so it removes its interface and routes, and kill it if
/// it does not.
async fn stop(child: &mut Child) {
    if let Some(pid) = child.id() {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_ok() {
            return;
        }
        tracing::warn!(pid, "nebula did not exit; killing it");
    }
    let _ = child.kill().await;
}
