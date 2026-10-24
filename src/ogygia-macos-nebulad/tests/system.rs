//! The installed app end to end, as root: launchd runs the bundled daemon,
//! which brings up a real utun and joins a throwaway mesh. The test pings
//! across it, forces a peer behind a relay with pf, checks a non-admin is
//! refused the mesh, renders the settings window from the live daemon, and
//! replaces the daemon the way an update does.
//!
//! It changes the firewall and runs a root daemon, so it refuses to run
//! anywhere but a disposable virtual machine with no Ogygia installed: CI's
//! macOS runners, or a VM of your own. Run it with
//! `nix run .#test-macos-system -- <Ogygia.app>`; see app/macos/TESTING.md.
//! It uses `sudo -n` for the steps that need root.

#![cfg(target_os = "macos")]

mod common;

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use common::*;
use ogygia_macos_nebulad::config::ConfigMode;
use ogygia_macos_nebulad::config::LocalConfig;
use ogygia_macos_nebulad::mesh::PeerState;
use ogygia_macos_nebulad::protocol::Nebula;
use ogygia_macos_nebulad::protocol::Request;
use ogygia_macos_nebulad::protocol::Response;
use ogygia_macos_nebulad::protocol::SOCKET_PATH;

const LABEL: &str = "dev.ogygia.macos.nebulad";
const PLIST: &str = "/Library/LaunchDaemons/dev.ogygia.macos.nebulad.plist";
/// Where the test installs its copy of the app, owned by root as an
/// installed app is.
const INSTALL: &str = "/Library/Ogygia-test";
const STATE: &str = "/Library/Application Support/Ogygia";
const LOG: &str = "/Library/Logs/ogygia-macos-nebulad.log";
/// The daemon looks for an update every minute, and launchd may wait ten
/// seconds before starting it again.
const RESTART: Duration = Duration::from_secs(150);

fn sudo(args: &[&str]) -> std::process::Output {
    run(Command::new("sudo").arg("-n").args(args))
}

/// Run the test only where nothing real can be harmed.
fn refuse_unless_disposable() {
    let vm = Command::new("sysctl")
        .args(["-n", "kern.hv_vmm_present"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&vm.stdout).trim(),
        "1",
        "this test runs a root daemon and changes the firewall, so it runs only in a virtual machine"
    );
    let installed = Command::new("launchctl")
        .args(["print", &format!("system/{LABEL}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(
        !installed && !Path::new(PLIST).exists() && !Path::new("/Applications/Ogygia.app").exists(),
        "Ogygia is installed on this machine; the test needs one without it"
    );
}

/// Undo what the test did to the machine, whatever happened.
struct Installed {
    output: PathBuf,
}

impl Drop for Installed {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["-n", "launchctl", "bootout", &format!("system/{LABEL}")])
            .status();
        let _ = Command::new("sudo")
            .args(["-n", "pfctl", "-F", "all", "-d"])
            .status();
        if let Ok(log) = Command::new("sudo").args(["-n", "cat", LOG]).output() {
            let _ = std::fs::write(self.output.join("daemon.log"), log.stdout);
        }
        let _ = Command::new("sudo")
            .args(["-n", "rm", "-rf", PLIST, INSTALL, STATE, LOG])
            .status();
    }
}

fn daemon_pid() -> Option<u32> {
    let output = Command::new("sudo")
        .args(["-n", "launchctl", "print", &format!("system/{LABEL}")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid = ")?.parse().ok())
}

/// Whether a ping across the mesh is answered.
fn pings(address: &str) -> bool {
    Command::new("ping")
        .args(["-c", "1", "-t", "2", address])
        .stdout(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
#[ignore = "runs a root daemon: only in a VM, with `nix run .#test-macos-system`"]
async fn the_installed_app_joins_a_mesh() {
    refuse_unless_disposable();
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let app = PathBuf::from(std::env::var("OGYGIA_TEST_APP").expect("OGYGIA_TEST_APP is not set"));

    sudo(&["mkdir", "-p", INSTALL]);
    let installed_app = Path::new(INSTALL).join("Ogygia.app");
    sudo(&[
        "ditto",
        app.to_str().unwrap(),
        installed_app.to_str().unwrap(),
    ]);
    let bin = installed_app.join("Contents/MacOS");
    let exe = bin.join("ogygia-macos-nebulad");

    let mut fixture = Fixture::new("system", bin.clone());
    let _installed = Installed {
        output: fixture.dir.clone(),
    };
    fixture.start_lighthouse();
    fixture.start_relay();
    fixture.start_nas();

    // The app registers this plist with SMAppService, which needs a person
    // to approve it; launchd is given the same job directly instead.
    let plist = fixture.path("daemon.plist");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/macos/LaunchDaemons")
            .join(format!("{LABEL}.plist")),
        &plist,
    )
    .unwrap();
    let plutil = |args: &[&str]| run(Command::new("plutil").args(args).arg(&plist));
    plutil(&["-remove", "BundleProgram"]);
    plutil(&["-remove", "AssociatedBundleIdentifiers"]);
    plutil(&["-insert", "Program", "-string", exe.to_str().unwrap()]);
    sudo(&["cp", plist.to_str().unwrap(), PLIST]);
    sudo(&["chown", "root:wheel", PLIST]);
    sudo(&["launchctl", "bootstrap", "system", PLIST]);

    let socket = Path::new(SOCKET_PATH);
    let first = until(
        "the daemon's socket",
        SETTLE,
        async || match tokio::net::UnixStream::connect(socket).await {
            Ok(_) => Some(status(socket).await),
            Err(_) => None,
        },
        Option::is_some,
    )
    .await
    .unwrap();
    let public_key = first.public_key.expect("the daemon made no key");

    fixture.write_fleet(&public_key);
    let response = ask(
        socket,
        &Request::SetSettings {
            repository_url: fixture.repository_url(),
            branch: None,
            device: "mac".into(),
        },
    )
    .await;
    assert!(matches!(response, Response::Status(_)), "{response:?}");
    let running = until(
        "Nebula to run the fleet's config",
        EVALUATION,
        async || status(socket).await,
        |s| matches!(s.nebula, Nebula::Running { .. }) && s.last_update.is_some(),
    )
    .await;
    assert_eq!(running.update_error, None);
    assert_eq!(running.address.as_deref(), Some(MAC));

    // A real utun carries the mesh.
    let ifconfig = String::from_utf8(run(&mut Command::new("ifconfig")).stdout).unwrap();
    assert!(
        ifconfig.contains(&format!("inet {MAC} ")),
        "no utun has {MAC}:\n{ifconfig}"
    );
    let mesh = || mesh(socket);
    until("the lighthouse tunnel", SETTLE, mesh, |m| {
        peer(m, LIGHTHOUSE, PeerState::Connected).is_some()
    })
    .await
    .unwrap();
    until(
        "a ping to the lighthouse",
        SETTLE,
        async || pings(LIGHTHOUSE),
        |ok| *ok,
    )
    .await;

    // Fix this Mac's port, then block it from the NAS, which leaves the
    // relay as the only way between them.
    let mac_port = free_udp_port();
    let local = LocalConfig {
        listen_port: mac_port,
        ..running
            .fleet_config
            .clone()
            .expect("no fleet config reported")
    };
    let response = ask(
        socket,
        &Request::SetConfig {
            mode: ConfigMode::Local,
            local,
        },
    )
    .await;
    assert!(matches!(response, Response::Status(_)), "{response:?}");
    let rules = fixture.path("pf.conf");
    let nas = fixture.nas_port;
    std::fs::write(
        &rules,
        format!(
            "block drop quick on lo0 proto udp from any port {mac_port} to any port {nas}\n\
             block drop quick on lo0 proto udp from any port {nas} to any port {mac_port}\n"
        ),
    )
    .unwrap();
    sudo(&["pfctl", "-q", "-f", rules.to_str().unwrap()]);
    let _ = Command::new("sudo")
        .args(["-n", "pfctl", "-q", "-e"])
        .status();
    until("the lighthouse after the restart", SETTLE, mesh, |m| {
        peer(m, LIGHTHOUSE, PeerState::Connected).is_some()
    })
    .await
    .unwrap();
    until("a ping to the NAS", SETTLE, async || pings(NAS), |ok| *ok).await;
    let relayed = until("the NAS through the relay", SETTLE, mesh, |m| {
        peer(m, NAS, PeerState::Connected).is_some_and(|p| p.via == [RELAY])
    })
    .await
    .unwrap();
    assert!(relayed.hosts.iter().any(|p| p.address == RELAY && p.relay));

    // Anyone may read the status, but only an administrator the mesh.
    let refused = Command::new("sudo")
        .args([
            "-n",
            "-u",
            "nobody",
            "/usr/bin/nc",
            "-w",
            "3",
            "-U",
            SOCKET_PATH,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .and_then(|mut nc| {
            use std::io::Write;
            nc.stdin
                .take()
                .unwrap()
                .write_all(b"{\"command\":\"mesh\"}\n")?;
            nc.wait_with_output()
        })
        .unwrap();
    let refused = String::from_utf8_lossy(&refused.stdout);
    assert!(refused.contains("only an administrator"), "{refused}");

    // The settings window, drawn from what this daemon reports.
    let shots = fixture.path("screenshots");
    run(Command::new(installed_app.join("Contents/MacOS/Ogygia"))
        .arg("--snapshots")
        .arg(&shots)
        .arg("--live"));
    assert!(shots.join("live-nebulaMesh.png").exists());

    // An update replaces the daemon's executable; the old daemon exits and
    // launchd starts the new one, which picks up where it left off.
    let before = daemon_pid().expect("launchd reports no pid for the daemon");
    let replacement = format!("{}.new", exe.display());
    sudo(&["cp", exe.to_str().unwrap(), &replacement]);
    sudo(&["mv", "-f", &replacement, exe.to_str().unwrap()]);
    until(
        "a new daemon",
        RESTART,
        async || daemon_pid(),
        |pid| pid.is_some_and(|p| p != before),
    )
    .await;
    let after = until(
        "Nebula under the new daemon",
        SETTLE,
        async || match tokio::net::UnixStream::connect(socket).await {
            Ok(_) => Some(status(socket).await),
            Err(_) => None,
        },
        |s| {
            s.as_ref()
                .is_some_and(|s| matches!(s.nebula, Nebula::Running { .. }))
        },
    )
    .await
    .unwrap();
    assert_eq!(after.config_mode, ConfigMode::Local);
    until("the NAS again", SETTLE, async || pings(NAS), |ok| *ok).await;
}
