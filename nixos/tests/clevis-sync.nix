{ pkgs, lib, system, ogygiaModule }:

# Drives ogygia-clevis through the transitions its "better blob" rule is
# meant to produce, against three real Tang servers whose keys are fixed
# fixtures so their thumbprints can be spelled out in the spec:
#
# 1. A blob bound to tang1 alone picks up tang2 once it is reachable.
# 2. With tang2 down, nothing changes: a blob is never traded for one that
#    covers fewer reachable servers.
# 3. With tang2 still down and tang3 up, {tang1,tang3} beats {tang1,tang2}.
# 4. With everything up, the blob converges on the full spec and then stays.
# 5. After tang1 rotates its keys the old pin is unreachable, so the blob is
#    kept until the spec adopts the new thumbprint, and then re-bound.

let
  fixtures = ./clevis-fixtures;

  thp = {
    tang1 = "H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE";
    tang1-rotated = "Jpr5SiK-f32WeIoSqI80tskEW00cp0VhFi0KJXbKZWc";
    tang2 = "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms";
    tang3 = "rnu9jb4221ef6evQnE0BsPsOch-CLJ4gDvD-Vaz8DTE";
  };

  pin = name: { url = "http://${name}:7654"; thp = thp.${name}; };

  secretFile = "/var/lib/test/disk.jwe";

  jsonFormat = pkgs.formats.json { };

  # The spec after tang1's rotation, run directly rather than via the unit so
  # the same VM can exercise both specs.
  rotatedConfig = jsonFormat.generate "rotated.json" {
    secret_file = secretFile;
    spec.t = 1;
    spec.pins.tang = [
      { url = "http://tang1:7654"; thp = thp.tang1-rotated; }
      (pin "tang2")
      (pin "tang3")
    ];
  };

  # Serve keys from a directory the test script populates, so a server's
  # keys can be swapped out mid-test.
  tangServer = {
    services.tang = {
      enable = true;
      ipAddressAllow = [ "192.168.1.0/24" ];
    };
    systemd.services."tangd@".serviceConfig.ExecStart = lib.mkForce "${pkgs.tang}/libexec/tangd /var/lib/tang-keys";
    networking.firewall.allowedTCPPorts = [ 7654 ];
  };
in
pkgs.testers.nixosTest {
  name = "ogygia-clevis-sync";

  nodes = {
    tang1 = tangServer;
    tang2 = tangServer;
    tang3 = tangServer;

    client = {
      imports = [ ogygiaModule ];

      ogygia = {
        enable = true;
        domain = "test.local";
        clevis = {
          enable = true;
          inherit secretFile;
          spec = {
            t = 1;
            pins.tang = [ (pin "tang1") (pin "tang2") (pin "tang3") ];
          };
        };
      };

      environment.systemPackages = [ pkgs.clevis ];
    };
  };

  testScript = { nodes, ... }: ''
    import json

    binary = "${nodes.client.config.ogygia.clevis.package}/bin/ogygia-clevis"
    secret_file = "${secretFile}"

    def install_keys(server, name):
        server.succeed(f"install -d -m 755 /var/lib/tang-keys && install -m 644 ${fixtures}/{name}/*.jwk /var/lib/tang-keys/")

    def run(expect, config=None):
        if config is None:
            client.succeed("systemctl start ogygia-clevis.service")
            out = client.succeed("journalctl -u ogygia-clevis.service --no-pager -o cat")
        else:
            out = client.succeed(f"{binary} --config {config} 2>&1")
        assert expect in out, out
        assert client.succeed(f"clevis decrypt < {secret_file}") == "supersecret", "blob no longer decrypts"
        assert client.succeed(f"stat -c %a {secret_file}").strip() == "400", "blob permissions changed"
        assert client.succeed("ls -A /var/lib/test").strip() == "disk.jwe", "temporary file left behind"

    start_all()
    for server, name in [(tang1, "tang1"), (tang2, "tang2"), (tang3, "tang3")]:
        install_keys(server, name)
        server.wait_for_unit("tangd.socket")
    tang3.succeed("systemctl stop tangd.socket")

    client.wait_for_unit("multi-user.target")
    initial = json.dumps({"t": 1, "pins": {"tang": [${builtins.toJSON (pin "tang1")}]}})
    client.succeed(f"install -d -m 700 /var/lib/test && echo -n supersecret | clevis encrypt sss '{initial}' > {secret_file} && chmod 400 {secret_file}")

    with subtest("binds the newly reachable server"):
        run("replaced blob: bound to 2 reachable specced pins (was 1)")

    with subtest("does not regress when a bound server is down"):
        tang2.succeed("systemctl stop tangd.socket")
        run("keeping current blob: current blob covers 1 of 1 reachable specced pins", "/etc/ogygia/clevis.json")

    with subtest("swaps a down server for a reachable one"):
        tang3.succeed("systemctl start tangd.socket")
        run("replaced blob: bound to 2 reachable specced pins (was 1)", "/etc/ogygia/clevis.json")

    with subtest("converges on the full spec"):
        tang2.succeed("systemctl start tangd.socket")
        run("replaced blob: bound to 3 reachable specced pins (was 2)", "/etc/ogygia/clevis.json")

    with subtest("is stable once fully bound"):
        run("keeping current blob: current blob covers 3 of 3 reachable specced pins", "/etc/ogygia/clevis.json")

    with subtest("a rotated server is unreachable under its old thumbprint"):
        tang1.succeed("rm /var/lib/tang-keys/*")
        install_keys(tang1, "tang1-rotated")
        run("keeping current blob: current blob covers 2 of 2 reachable specced pins", "/etc/ogygia/clevis.json")

    with subtest("re-binds once the spec adopts the rotated key"):
        run("replaced blob: bound to 3 reachable specced pins (was 2)", "${rotatedConfig}")
  '';
}
