//! Control socket for triggering update cycles in the running daemon.
//!
//! The daemon is the sole owner of the repository clone, the system
//! profile, and the update schedule; manual updates ask it to run a
//! cycle now rather than running the engine themselves. Authorization
//! is delegated to filesystem permissions on the socket.

use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

pub const DEFAULT_SOCKET_PATH: &str = "/run/ogygia-updated/control.sock";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A command for the daemon, sent over the control socket. Each variant
/// carries only the data its operation needs.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    /// Reset the target to the configured branch tip and run a cycle now.
    Update,
}

/// The daemon's answer to a trigger: the result message of the cycle it
/// ran, or the reason it could not run one. Serialized as a JSON
/// `Result`, e.g. `{"Ok":"switched to abc"}` or `{"Err":"..."}`.
pub type Response = Result<String, String>;

/// Trigger an update cycle in the daemon listening on `socket_path` and
/// wait for it to finish, returning its result message. Errors if the
/// daemon cannot be reached or reports a failed cycle.
pub fn request_update(socket_path: &Path) -> Result<String> {
    let mut stream = UnixStream::connect(socket_path).with_context(|| {
        format!(
            "connecting to {}; is ogygia-updated running, and are you root?",
            socket_path.display()
        )
    })?;

    let mut request = serde_json::to_vec(&Request::Update)?;
    request.push(b'\n');
    stream
        .write_all(&request)
        .context("sending update request")?;

    // No read timeout: the cycle legitimately takes as long as a build.
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("reading daemon response")?;
    match serde_json::from_str::<Response>(&line).context("parsing daemon response")? {
        Ok(message) => Ok(message),
        Err(message) => bail!(message),
    }
}

/// Bind the control socket, replacing a stale one from a previous run
/// and restricting it to root.
pub fn bind(socket_path: &Path) -> Result<UnixListener> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("removing stale socket {}", socket_path.display()))?;
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("binding control socket {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
        .context("restricting control socket permissions")?;
    Ok(listener)
}

/// Accept trigger connections and hand them to the update loop, which
/// answers each with the result of the cycle it ran. Invalid requests
/// are answered immediately without triggering a cycle.
pub fn listen(listener: UnixListener, triggers: mpsc::Sender<UnixStream>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));

        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            continue;
        }
        if serde_json::from_str::<Request>(&line).is_err() {
            respond(&mut stream, Err("invalid request".to_string()));
            continue;
        }

        if triggers.send(stream).is_err() {
            return;
        }
    }
}

/// Answer a trigger connection. Failures only lose the reply, never the
/// update cycle, so they are logged and swallowed.
pub fn respond(stream: &mut UnixStream, response: Response) {
    let mut payload = match serde_json::to_vec(&response) {
        Ok(payload) => payload,
        Err(error) => {
            warn!(%error, "failed to encode control response");
            return;
        }
    };
    payload.push(b'\n');
    if let Err(error) = stream.write_all(&payload) {
        warn!(%error, "failed to answer on the control socket");
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn request_update_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket_path = dir.path().join("control.sock");
        let listener = bind(&socket_path).unwrap();

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || listen(listener, sender));
        let server = thread::spawn(move || {
            let mut stream = receiver.recv().unwrap();
            respond(&mut stream, Ok("switched to abc".to_string()));
            let mut stream = receiver.recv().unwrap();
            respond(&mut stream, Err("update failed".to_string()));
        });

        assert_eq!(request_update(&socket_path).unwrap(), "switched to abc");
        let error = request_update(&socket_path).unwrap_err();
        assert_eq!(error.to_string(), "update failed");
        server.join().unwrap();
    }

    #[test]
    fn stale_sockets_are_replaced_and_garbage_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket_path = dir.path().join("control.sock");
        drop(bind(&socket_path).unwrap());

        // A leftover socket file from a previous run is replaced.
        let listener = bind(&socket_path).unwrap();

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || listen(listener, sender));

        let mut stream = UnixStream::connect(&socket_path).unwrap();
        stream.write_all(b"not json\n").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let response: Response = serde_json::from_str(&line).unwrap();
        assert!(response.is_err());

        // The garbage request never reached the update loop.
        assert!(receiver.try_recv().is_err());
    }
}
