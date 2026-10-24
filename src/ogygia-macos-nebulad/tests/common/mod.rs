//! A throwaway mesh for the daemon's tests: a CA, a fleet repository the
//! daemon evaluates with real `nix`, and Nebula nodes on loopback with no tun
//! device, so none of them needs root.
//!
//! The fleet is the same in every test:
//!
//! | host   | address    | runs                                   |
//! |--------|------------|----------------------------------------|
//! | lh     | 10.42.0.1  | the lighthouse                         |
//! | relay  | 10.42.0.2  | a relay                                |
//! | nas    | 10.42.0.3  | a peer that accepts relayed tunnels     |
//! | mac    | 10.42.0.20 | the daemon under test                  |
//! | laptop | 10.42.0.21 | nothing: a host in the fleet, offline  |

#![allow(dead_code)]

use std::net::TcpListener;
use std::net::UdpSocket;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::time::Duration;

use ogygia_macos_nebulad::mesh::Mesh;
use ogygia_macos_nebulad::mesh::Peer;
use ogygia_macos_nebulad::mesh::PeerState;
use ogygia_macos_nebulad::protocol::Request;
use ogygia_macos_nebulad::protocol::Response;
use ogygia_macos_nebulad::protocol::Status;
use serde_json::Value;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;

/// The first evaluation copies nixpkgs' lib; later ones are quick.
pub const EVALUATION: Duration = Duration::from_secs(180);
/// Everything else happens within seconds.
pub const SETTLE: Duration = Duration::from_secs(30);

pub const LIGHTHOUSE: &str = "10.42.0.1";
pub const RELAY: &str = "10.42.0.2";
pub const NAS: &str = "10.42.0.3";
pub const MAC: &str = "10.42.0.20";
pub const LAPTOP: &str = "10.42.0.21";

pub struct Fixture {
    /// Removed on drop, unless the run was asked to keep its files.
    _temp: Option<tempfile::TempDir>,
    pub dir: PathBuf,
    pub nebula: PathBuf,
    nixpkgs: String,
    pub lighthouse_port: u16,
    pub relay_port: u16,
    pub nas_port: u16,
    nodes: Vec<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for node in &mut self.nodes {
            let _ = node.kill();
            let _ = node.wait();
        }
    }
}

impl Fixture {
    /// A new CA in a fresh directory. `OGYGIA_TEST_NIXPKGS` names the nixpkgs
    /// whose `lib` the fleet uses; with `OGYGIA_TEST_OUTPUT` set, the files
    /// and logs are kept under it, named `name`, for CI to upload.
    pub fn new(name: &str, nebula: PathBuf) -> Self {
        let nixpkgs = std::env::var("OGYGIA_TEST_NIXPKGS").expect("OGYGIA_TEST_NIXPKGS is not set");
        let (temp, dir) = match std::env::var_os("OGYGIA_TEST_OUTPUT") {
            Some(output) => {
                let dir = PathBuf::from(output).join(name);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                (None, dir)
            }
            None => {
                let temp = tempfile::tempdir().unwrap();
                let dir = temp.path().to_owned();
                (Some(temp), dir)
            }
        };
        let fixture = Self {
            _temp: temp,
            dir,
            nebula,
            nixpkgs,
            lighthouse_port: free_udp_port(),
            relay_port: free_udp_port(),
            nas_port: free_udp_port(),
            nodes: Vec::new(),
        };
        std::fs::create_dir_all(fixture.fleet().join("certs")).unwrap();
        fixture.nebula_cert(&[
            "ca",
            "-name",
            "ogygia-test",
            "-out-crt",
            "certs/ca.crt",
            "-out-key",
            "../ca.key",
        ]);
        fixture
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn fleet(&self) -> PathBuf {
        self.path("fleet")
    }

    fn nebula_cert(&self, args: &[&str]) {
        run(Command::new(self.nebula.join("nebula-cert"))
            .args(args)
            .current_dir(self.fleet()));
    }

    fn lighthouse_endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.lighthouse_port)
    }

    pub fn start_lighthouse(&mut self) {
        let port = self.lighthouse_port;
        self.start_node(
            "lh",
            LIGHTHOUSE,
            json!({
                "lighthouse": { "am_lighthouse": true },
                "listen": { "host": "127.0.0.1", "port": port },
            }),
        );
    }

    pub fn start_relay(&mut self) {
        let port = self.relay_port;
        let lighthouse = self.lighthouse_endpoint();
        self.start_node(
            "relay",
            RELAY,
            json!({
                "static_host_map": { LIGHTHOUSE: [lighthouse] },
                "lighthouse": { "hosts": [LIGHTHOUSE] },
                "relay": { "am_relay": true },
                "listen": { "host": "127.0.0.1", "port": port },
            }),
        );
    }

    pub fn start_nas(&mut self) {
        let port = self.nas_port;
        let lighthouse = self.lighthouse_endpoint();
        self.start_node(
            "nas",
            NAS,
            json!({
                "static_host_map": { LIGHTHOUSE: [lighthouse] },
                "lighthouse": { "hosts": [LIGHTHOUSE] },
                "relay": { "use_relays": true, "relays": [RELAY] },
                "listen": { "host": "127.0.0.1", "port": port },
            }),
        );
    }

    /// A Nebula node with its own cert, running unprivileged with no tun.
    fn start_node(&mut self, name: &str, address: &str, settings: Value) {
        let crt = format!("../{name}.crt");
        let key = format!("../{name}.key");
        self.nebula_cert(&[
            "sign",
            "-ca-crt",
            "certs/ca.crt",
            "-ca-key",
            "../ca.key",
            "-name",
            name,
            "-networks",
            &format!("{address}/24"),
            "-out-crt",
            &crt,
            "-out-key",
            &key,
        ]);
        let mut config = json!({
            "pki": {
                "ca": self.fleet().join("certs/ca.crt"),
                "cert": self.path(&format!("{name}.crt")),
                "key": self.path(&format!("{name}.key")),
            },
            "tun": { "disabled": true },
            "firewall": {
                "inbound": [{ "port": "any", "proto": "any", "host": "any" }],
                "outbound": [{ "port": "any", "proto": "any", "host": "any" }],
            },
        });
        merge(&mut config, settings);
        let path = self.path(&format!("{name}.json"));
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let log = std::fs::File::create(self.path(&format!("{name}.log"))).unwrap();
        let child = Command::new(self.nebula.join("nebula"))
            .arg("-config")
            .arg(&path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.nodes.push(child);
    }

    /// The fleet repository, with this Mac enrolled under `public_key` and
    /// its cert signed for whatever spec the module derives.
    pub fn write_fleet(&self, public_key: &str) {
        let fleet = self.fleet();
        let module_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nixos/nebula");
        std::fs::create_dir_all(fleet.join("ogygia")).unwrap();
        for file in ["common.nix", "device.nix"] {
            std::fs::copy(module_src.join(file), fleet.join("ogygia").join(file)).unwrap();
        }
        std::fs::write(fleet.join("mac.pub"), public_key).unwrap();
        std::fs::write(
            fleet.join("flake.nix"),
            format!(
                r#"{{
  inputs.nixpkgs.url = "path:{nixpkgs}";
  outputs = {{ nixpkgs, ... }}: {{
    ogygiaConfigurations.mac = import ./ogygia/device.nix {{
      name = "mac";
      inherit (nixpkgs) lib;
      modules = [{{
        ogygia.nebula = {{
          pubKey = builtins.readFile ./mac.pub;
          certDir = ./certs;
          firewall.inbound = [{{ port = "any"; proto = "icmp"; host = "any"; }}];
          topology = {{
            subnet = "10.42.0.0/24";
            hosts = {{
              lh = {{ ipv4 = "{LIGHTHOUSE}"; endpoint = "{lighthouse}"; }};
              relay.ipv4 = "{RELAY}";
              nas.ipv4 = "{NAS}";
              mac.ipv4 = "{MAC}";
              laptop.ipv4 = "{LAPTOP}";
            }};
            lighthouses = [ "lh" ];
            relays = [ "relay" ];
          }};
        }};
      }}];
    }};
  }};
}}
"#,
                nixpkgs = self.nixpkgs,
                lighthouse = self.lighthouse_endpoint(),
            ),
        )
        .unwrap();
        let git = |args: &[&str]| {
            run(Command::new("git")
                .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
                .args(args)
                .current_dir(&fleet));
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "fleet"]);

        let spec_hash = String::from_utf8(
            run(Command::new("nix")
                .args([
                    "eval",
                    "--raw",
                    "--extra-experimental-features",
                    "nix-command flakes",
                ])
                .arg(format!(
                    "git+file://{}#ogygiaConfigurations.mac.config.ogygia.nebula.specHash",
                    fleet.display()
                )))
            .stdout,
        )
        .unwrap();
        self.nebula_cert(&[
            "sign",
            "-ca-crt",
            "certs/ca.crt",
            "-ca-key",
            "../ca.key",
            "-name",
            "mac",
            "-networks",
            &format!("{MAC}/24"),
            "-in-pub",
            "mac.pub",
            "-out-crt",
            &format!("certs/{spec_hash}.crt"),
        ]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "sign mac"]);
    }

    pub fn repository_url(&self) -> String {
        format!("file://{}", self.fleet().display())
    }
}

pub fn run(command: &mut Command) -> std::process::Output {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn merge(base: &mut Value, extra: Value) {
    match (base, extra) {
        (Value::Object(base), Value::Object(extra)) => {
            for (k, v) in extra {
                merge(base.entry(k).or_insert(Value::Null), v);
            }
        }
        (base, extra) => *base = extra,
    }
}

pub fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub async fn ask(socket: &Path, request: &Request) -> Response {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    write.write_all(&line).await.unwrap();
    let mut reply = String::new();
    BufReader::new(read).read_line(&mut reply).await.unwrap();
    serde_json::from_str(&reply).unwrap()
}

pub async fn status(socket: &Path) -> Status {
    match ask(socket, &Request::Status).await {
        Response::Status(status) => *status,
        other => panic!("expected a status, got {other:?}"),
    }
}

pub async fn mesh(socket: &Path) -> Result<Mesh, String> {
    match ask(socket, &Request::Mesh).await {
        Response::Mesh(mesh) => Ok(mesh),
        Response::Error(e) => Err(e),
        Response::Status(_) => panic!("a status in reply to the mesh"),
    }
}

/// The host at `address`, once the mesh has it in `state`.
pub fn peer(mesh: &Result<Mesh, String>, address: &str, state: PeerState) -> Option<Peer> {
    mesh.as_ref()
        .ok()?
        .hosts
        .iter()
        .find(|p| p.address == address && p.state == state)
        .cloned()
}

/// Ask until `check` accepts the answer, or fail with the last one.
pub async fn until<T: std::fmt::Debug>(
    what: &str,
    within: Duration,
    mut ask: impl AsyncFnMut() -> T,
    check: impl Fn(&T) -> bool,
) -> T {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let answer = ask().await;
        if check(&answer) {
            return answer;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("timed out waiting for {what}; last answer: {answer:#?}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
