//! The daemon end to end, unprivileged: a generated fleet repository and CA,
//! real `nix eval`, and real Nebula nodes, all without a tun device, on
//! loopback, in a temporary directory. Nothing here needs root or touches
//! the Mac's own daemon, so it is safe to run on any Mac.
//!
//! It needs `nix`, `git` and Nebula, so `cargo test` skips it; run it with
//! `nix run .#test-macos-daemon`, which provides them through
//! `OGYGIA_TEST_NEBULA` (the directory holding `nebula` and `nebula-cert`)
//! and `OGYGIA_TEST_NIXPKGS` (a nixpkgs source tree, for its `lib`).

#![cfg(target_os = "macos")]

mod common;

use std::path::PathBuf;

use common::*;
use ogygia_macos_nebulad::config::ConfigMode;
use ogygia_macos_nebulad::config::LocalConfig;
use ogygia_macos_nebulad::daemon;
use ogygia_macos_nebulad::mesh::PeerState;
use ogygia_macos_nebulad::preferences::FilePreferences;
use ogygia_macos_nebulad::protocol::Nebula;
use ogygia_macos_nebulad::protocol::Request;
use ogygia_macos_nebulad::protocol::Response;
use ogygia_macos_nebulad::protocol::Status;
use serde_json::Value;
use serde_json::json;
use tokio::net::UnixStream;

fn rendered(fixture: &Fixture) -> Value {
    serde_json::from_slice(&std::fs::read(fixture.path("state/config.yml")).unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "needs nix, git and Nebula: run with `nix run .#test-macos-daemon`"]
async fn the_daemon_joins_a_mesh_and_reports_it() {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let nebula =
        PathBuf::from(std::env::var("OGYGIA_TEST_NEBULA").expect("OGYGIA_TEST_NEBULA is not set"));
    let mut fixture = Fixture::new("daemon", nebula);
    let lighthouse = fixture.lighthouse_port;
    fixture.start_lighthouse();
    fixture.start_nas();

    let socket = fixture.path("daemon.sock");
    let sshd_port = free_tcp_port();
    let env = daemon::Env {
        state_dir: fixture.path("state"),
        socket: socket.clone(),
        bin_dir: fixture.nebula.clone(),
        sshd_port,
        preferences: Box::new(FilePreferences {
            path: fixture.path("preferences.json"),
        }),
        tun_disabled: true,
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let daemon = tokio::spawn(daemon::run_until(env, async {
        let _ = stopped.await;
    }));

    // Unconfigured, it runs nothing but has made this Mac's key.
    let first = until(
        "the socket",
        SETTLE,
        async || match UnixStream::connect(&socket).await {
            Ok(_) => Some(status(&socket).await),
            Err(_) => None,
        },
        Option::is_some,
    )
    .await
    .unwrap();
    assert_eq!(first.nebula, Nebula::Stopped);
    assert_eq!(first.config_mode, ConfigMode::Fleet);
    let public_key = first.public_key.expect("the daemon made no key");
    assert!(public_key.contains("NEBULA"), "{public_key}");

    fixture.write_fleet(&public_key);
    let response = ask(
        &socket,
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
        async || status(&socket).await,
        |s| matches!(s.nebula, Nebula::Running { .. }) && s.last_update.is_some(),
    )
    .await;
    assert_eq!(running.update_error, None);
    assert_eq!(running.address.as_deref(), Some("10.42.0.20"));
    let fleet_config = running
        .fleet_config
        .clone()
        .expect("no fleet config reported");
    assert_eq!(fleet_config.lighthouses[0].address, "10.42.0.1");
    assert_eq!(
        fleet_config.lighthouses[0].endpoints,
        [format!("127.0.0.1:{lighthouse}")]
    );

    // The daemon's own sshd is in the config, on loopback only.
    let config = rendered(&fixture);
    assert_eq!(
        config["sshd"]["listen"],
        json!(format!("127.0.0.1:{sshd_port}"))
    );
    assert_eq!(config["tun"], json!({ "disabled": true }));
    assert_eq!(config["pki"]["key"], json!(fixture.path("state/host.key")));

    let mesh = || mesh(&socket);
    let reported = until("the lighthouse tunnel", SETTLE, mesh, |m| {
        peer(m, LIGHTHOUSE, PeerState::Connected).is_some()
    })
    .await
    .unwrap();
    let lh = reported
        .hosts
        .iter()
        .find(|p| p.address == "10.42.0.1")
        .unwrap();
    assert!(lh.lighthouse);
    assert_eq!(
        lh.remote.as_deref(),
        Some(format!("127.0.0.1:{lighthouse}").as_str())
    );
    assert!(lh.via.is_empty());
    let laptop = reported
        .hosts
        .iter()
        .find(|p| p.address == "10.42.0.21")
        .unwrap();
    assert_eq!(
        (laptop.name.as_deref(), laptop.state),
        (Some("laptop"), PeerState::NoTunnel)
    );
    assert!(reported.hosts.iter().any(|p| p.address == "10.42.0.3"));
    assert!(
        reported.hosts.iter().all(|p| p.address != "10.42.0.20"),
        "this Mac is listed"
    );

    // Managing locally replaces the fleet's values, and Nebula restarts on
    // the new config.
    let since = |s: &Status| match s.nebula {
        Nebula::Running { since } => Some(since),
        _ => None,
    };
    let before = since(&running);
    let port = free_udp_port();
    let local = LocalConfig {
        listen_port: port,
        ..fleet_config.clone()
    };
    let response = ask(
        &socket,
        &Request::SetConfig {
            mode: ConfigMode::Local,
            local: local.clone(),
        },
    )
    .await;
    let Response::Status(after) = response else {
        panic!("{response:?}");
    };
    assert_eq!(after.config_mode, ConfigMode::Local);
    assert_eq!(after.local_config, local);
    assert_eq!(rendered(&fixture)["listen"]["port"], json!(port));
    until(
        "Nebula to restart on the local config",
        SETTLE,
        async || status(&socket).await,
        |s| since(s).is_some() && since(s) != before,
    )
    .await;
    until("the lighthouse tunnel again", SETTLE, mesh, |m| {
        peer(m, LIGHTHOUSE, PeerState::Connected).is_some()
    })
    .await
    .unwrap();

    // A local config Nebula would misread is refused and changes nothing.
    let mut bad = local.clone();
    bad.relays = vec!["not-an-address".into()];
    let response = ask(
        &socket,
        &Request::SetConfig {
            mode: ConfigMode::Local,
            local: bad,
        },
    )
    .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert_eq!(rendered(&fixture)["listen"]["port"], json!(port));

    // Following the fleet again restores its values.
    let response = ask(
        &socket,
        &Request::SetConfig {
            mode: ConfigMode::Fleet,
            local,
        },
    )
    .await;
    assert!(matches!(response, Response::Status(_)), "{response:?}");
    assert_eq!(rendered(&fixture)["listen"]["port"], json!(0));

    let _ = stop.send(());
    daemon.await.unwrap().unwrap();
    assert!(!socket.exists(), "the daemon left its socket behind");
}
