//! The daemon itself: key, update loop, Nebula and the socket.

use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use chrono::Utc;
use core_foundation::bundle::CFBundle;
use core_foundation::string::CFString;
use ogygia_nixutils::Nix;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::signal::unix::SignalKind;
use tokio::signal::unix::signal;
use tokio::sync::Notify;
use tokio::sync::watch;

use crate::peer;
use crate::prefs;
use crate::protocol::Request;
use crate::protocol::Response;
use crate::protocol::SOCKET_PATH;
use crate::protocol::Status;
use crate::render;
use crate::report;
use crate::settings;
use crate::supervisor::Supervisor;

/// Root-only state: the key, and the last config that evaluated.
const STATE_DIR: &str = "/Library/Application Support/Ogygia/Nebula";
/// How often to look for a new config, as `ogygia-updated` polls on NixOS.
const UPDATE_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// A first evaluation fetches nixpkgs, so allow it a while.
const EVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How often to check whether an app update has replaced the daemon.
const REPLACED_CHECK: Duration = Duration::from_secs(60);

struct Paths {
    exe: PathBuf,
    key: PathBuf,
    public_key: PathBuf,
    config: PathBuf,
    cert: PathBuf,
    nebula: PathBuf,
    nebula_cert: PathBuf,
}

impl Paths {
    /// Nebula ships in the app bundle beside the daemon, so the app's version
    /// of Nebula is always the one that runs.
    fn new() -> Result<Self> {
        let exe = std::env::current_exe().context("could not find the daemon's own path")?;
        let bin = exe.parent().context("the daemon has no parent directory")?;
        let state = Path::new(STATE_DIR);
        Ok(Self {
            exe: exe.clone(),
            key: state.join("host.key"),
            public_key: state.join("host.pub"),
            config: state.join("config.yml"),
            cert: state.join("host.crt"),
            nebula: bin.join("nebula"),
            nebula_cert: bin.join("nebula-cert"),
        })
    }
}

struct Daemon {
    paths: Paths,
    nix: Nix,
    status: Arc<Mutex<Status>>,
    refresh: Notify,
    changes: watch::Sender<u64>,
}

pub async fn run() -> Result<()> {
    let paths = Paths::new()?;
    std::fs::create_dir_all(STATE_DIR).with_context(|| format!("could not create {STATE_DIR}"))?;
    std::fs::set_permissions(STATE_DIR, std::fs::Permissions::from_mode(0o700))?;

    let public_key = ensure_key(&paths).await?;
    let mut status = Status {
        build: build(),
        public_key: Some(public_key),
        ..Status::default()
    };
    if let Ok(info) = cert_info(&paths).await {
        status.address = info.address;
        status.cert_expires = Some(info.not_after);
    }

    let (changes, changes_rx) = watch::channel(0u64);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let daemon = Arc::new(Daemon {
        nix: Nix::with_eval_args(
            [
                "--extra-experimental-features",
                "nix-command flakes",
                // Fetch the repository afresh rather than trusting a cached
                // copy, so a pushed cert reaches the Mac on the next update.
                "--refresh",
            ]
            .map(String::from),
        ),
        status: Arc::new(Mutex::new(status)),
        refresh: Notify::new(),
        changes,
        paths,
    });

    // The last good config starts at once, so the mesh is up at boot even
    // when the repository cannot be reached.
    let supervisor = tokio::spawn(
        Supervisor {
            nebula: daemon.paths.nebula.clone(),
            config: daemon.paths.config.clone(),
            status: daemon.status.clone(),
        }
        .run(changes_rx, shutdown_rx),
    );

    let listener = listen()?;
    tokio::spawn(serve(daemon.clone(), listener));
    tokio::spawn(update_loop(daemon.clone()));

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => tracing::info!("stopping"),
        _ = int.recv() => tracing::info!("stopping"),
        // launchd keeps running the old binary after Sparkle replaces the
        // app, so exit and let KeepAlive start the new one from the bundle.
        _ = replaced(daemon.paths.exe.clone()) => {
            tracing::info!("an update replaced the daemon; exiting so launchd starts the new one");
        }
    }
    let _ = shutdown.send(true);
    let _ = supervisor.await;
    let _ = std::fs::remove_file(SOCKET_PATH);
    Ok(())
}

/// The app build this daemon came from, from the bundle it sits in.
fn build() -> Option<String> {
    let info = CFBundle::main_bundle().info_dictionary();
    let version = info.find(CFString::from_static_string("CFBundleVersion"))?;
    version.downcast::<CFString>().map(|v| v.to_string())
}

/// Resolve once the daemon's executable is no longer the file it started
/// from: the app it ships in was updated or reinstalled. A missing file is
/// an update still in progress, so only a new file counts.
async fn replaced(exe: PathBuf) {
    let identity = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .map(|m| (m.dev(), m.ino(), m.mtime(), m.mtime_nsec()))
    };
    let Some(original) = identity(&exe) else {
        tracing::warn!(exe = %exe.display(), "cannot watch the daemon's own executable for updates");
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(REPLACED_CHECK).await;
        if identity(&exe).is_some_and(|now| now != original) {
            return;
        }
    }
}

/// The Mac's Nebula key, generated on first start. Only its public half ever
/// leaves this directory.
async fn ensure_key(paths: &Paths) -> Result<String> {
    if !paths.key.exists() || !paths.public_key.exists() {
        tracing::info!("generating this Mac's Nebula key");
        let _ = std::fs::remove_file(&paths.key);
        let _ = std::fs::remove_file(&paths.public_key);
        let output = Command::new(&paths.nebula_cert)
            .arg("keygen")
            .arg("-out-key")
            .arg(&paths.key)
            .arg("-out-pub")
            .arg(&paths.public_key)
            .output()
            .await
            .context("could not run nebula-cert keygen")?;
        if !output.status.success() {
            bail!(
                "nebula-cert keygen failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    std::fs::set_permissions(&paths.key, std::fs::Permissions::from_mode(0o600))?;
    std::fs::read_to_string(&paths.public_key)
        .with_context(|| format!("could not read {}", paths.public_key.display()))
}

async fn update_loop(daemon: Arc<Daemon>) {
    loop {
        daemon.update().await;
        tokio::select! {
            _ = tokio::time::sleep(UPDATE_INTERVAL) => {}
            _ = daemon.refresh.notified() => {}
        }
    }
}

impl Daemon {
    async fn update(&self) {
        let settings = prefs::load();
        {
            let mut status = self.status.lock().unwrap();
            status.repository_url = settings.repository_url.clone();
            status.branch = settings.branch.clone();
            status.device = settings.device.clone();
            status.settings_managed = prefs::managed();
        }

        let installable = match settings.installable() {
            Ok(Some(installable)) => installable,
            Ok(None) => {
                // Unconfigured means off: nothing says what to trust.
                self.set_config(None).await;
                self.status.lock().unwrap().update_error = None;
                return;
            }
            Err(e) => {
                self.status.lock().unwrap().update_error = Some(e.to_string());
                return;
            }
        };

        tracing::info!(%installable, "evaluating");
        self.status.lock().unwrap().updating = true;
        let result = self.evaluate(&installable).await;
        let mut status = self.status.lock().unwrap();
        status.updating = false;
        match result {
            Ok(()) => {
                status.last_update = Some(Utc::now());
                status.update_error = None;
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "update failed");
                status.update_error = Some(report::nix_error(&format!("{e:#}")));
            }
        }
    }

    async fn evaluate(&self, installable: &str) -> Result<()> {
        let settings: Value =
            tokio::time::timeout(EVAL_TIMEOUT, self.nix.eval_json(installable, None))
                .await
                .context("nix eval timed out")??;
        let config = render::render(settings, &self.paths.key)?;
        self.set_config(Some(config)).await;
        Ok(())
    }

    /// Make `config` the one Nebula runs, or stop Nebula for `None`, and
    /// restart Nebula only if that changed anything.
    async fn set_config(&self, config: Option<Vec<u8>>) {
        let current = std::fs::read(&self.paths.config).ok();
        if current == config {
            return;
        }
        let written = match &config {
            Some(config) => write_atomically(&self.paths.config, config).and_then(|()| {
                let cert = render::certificate(config).unwrap_or_default();
                write_atomically(&self.paths.cert, cert.as_bytes())
            }),
            None => std::fs::remove_file(&self.paths.config).map_err(Into::into),
        };
        if let Err(e) = written {
            self.status.lock().unwrap().update_error = Some(format!("{e:#}"));
            return;
        }

        let info = match config {
            Some(_) => cert_info(&self.paths).await.ok(),
            None => None,
        };
        {
            let mut status = self.status.lock().unwrap();
            status.address = info.as_ref().and_then(|i| i.address.clone());
            status.cert_expires = info.map(|i| i.not_after);
        }
        self.changes.send_modify(|generation| *generation += 1);
    }
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("could not replace {}", path.display()))
}

async fn cert_info(paths: &Paths) -> Result<report::CertInfo> {
    let output = Command::new(&paths.nebula_cert)
        .arg("print")
        .arg("-json")
        .arg("-path")
        .arg(&paths.cert)
        .output()
        .await
        .context("could not run nebula-cert print")?;
    if !output.status.success() {
        bail!(
            "nebula-cert print failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    report::cert_info(&output.stdout)
}

/// Anyone on the Mac may read the status, so the socket is world-writable;
/// changing settings is checked per connection instead.
fn listen() -> Result<UnixListener> {
    let socket = Path::new(SOCKET_PATH);
    let dir = socket.parent().context("socket path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    let _ = std::fs::remove_file(socket);
    let listener =
        UnixListener::bind(socket).with_context(|| format!("could not listen on {SOCKET_PATH}"))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

async fn serve(daemon: Arc<Daemon>, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(daemon.clone().handle(stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

impl Daemon {
    async fn handle(self: Arc<Self>, stream: UnixStream) {
        let admin = peer::is_admin(&stream);
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let response = match serde_json::from_str::<Request>(&line) {
                Ok(request) => self.respond(request, admin),
                Err(e) => Response::Error(format!("bad request: {e}")),
            };
            let mut out = match serde_json::to_vec(&response) {
                Ok(out) => out,
                Err(_) => return,
            };
            out.push(b'\n');
            if write.write_all(&out).await.is_err() {
                return;
            }
        }
    }

    fn respond(&self, request: Request, admin: bool) -> Response {
        match request {
            Request::Status => {}
            Request::Refresh => self.refresh.notify_one(),
            Request::SetSettings {
                repository_url,
                branch,
                device,
            } => {
                let settings = settings::Settings {
                    repository_url: Some(repository_url.trim().to_owned()),
                    branch: branch
                        .map(|b| b.trim().to_owned())
                        .filter(|b| !b.is_empty()),
                    device: Some(device.trim().to_owned()),
                };
                if let Err(e) = self.set_settings(settings, admin) {
                    return Response::Error(format!("{e:#}"));
                }
                self.refresh.notify_one();
            }
        }
        Response::Status(self.status.lock().unwrap().clone())
    }

    fn set_settings(&self, settings: settings::Settings, admin: bool) -> Result<()> {
        if !admin {
            bail!("only an administrator can change which fleet this Mac follows");
        }
        if prefs::managed() {
            bail!("these settings are managed by a configuration profile");
        }
        settings.validate()?;
        prefs::store(&settings)?;

        let mut status = self.status.lock().unwrap();
        status.repository_url = settings.repository_url;
        status.branch = settings.branch;
        status.device = settings.device;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Install a new file at `path` the way an update does: write beside it
    /// and rename over it.
    fn reinstall(path: &Path) {
        let new = path.with_extension("new");
        std::fs::write(&new, b"new build").unwrap();
        std::fs::rename(&new, path).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_reinstalled_executable_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ogygia-macos-nebulad");
        std::fs::write(&exe, b"old build").unwrap();

        let watch = tokio::spawn(replaced(exe.clone()));
        tokio::time::sleep(REPLACED_CHECK * 2).await;
        assert!(
            !watch.is_finished(),
            "an untouched executable is not an update"
        );

        reinstall(&exe);
        tokio::time::timeout(REPLACED_CHECK * 2, watch)
            .await
            .expect("the update was not noticed")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_update_in_progress_is_waited_out() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ogygia-macos-nebulad");
        std::fs::write(&exe, b"old build").unwrap();

        let watch = tokio::spawn(replaced(exe.clone()));
        tokio::task::yield_now().await;
        std::fs::remove_file(&exe).unwrap();
        tokio::time::sleep(REPLACED_CHECK * 3).await;
        assert!(
            !watch.is_finished(),
            "a missing executable is not yet an update"
        );

        reinstall(&exe);
        tokio::time::timeout(REPLACED_CHECK * 2, watch)
            .await
            .expect("the finished update was not noticed")
            .unwrap();
    }
}
