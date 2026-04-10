{ pkgs, lib, ogygiaModule }:

let
  # Test etcd only configuration
  etcdOnlySystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.etcd = {
          enable = true;
          endpoints = [ "http://etcd1.internal:2379" "http://etcd2.internal:2379" ];
          namespace = "/cluster/nixos";
          timeoutSeconds = 42;
        };
      }
    ];
  };

  etcdOnlyConfigPackage = etcdOnlySystem.config.ogygia.cliConfig.package or
    (throw ''Ogygia CLI config package not found. Ensure ogygia.cliConfig.enable is true when ogygia.enable = true.'');

  # Test zookeeper only configuration
  zkOnlySystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.zookeeper = {
          enable = true;
          endpoints = [ "zk1.internal:2181" "zk2.internal:2181" ];
          namespace = "/cluster/nixos";
          timeoutSeconds = 42;
        };
      }
    ];
  };

  zkOnlyConfigPackage = zkOnlySystem.config.ogygia.cliConfig.package or
    (throw ''Ogygia CLI config package not found. Ensure ogygia.cliConfig.enable is true when ogygia.enable = true.'');

  # Test both etcd and zookeeper configuration
  bothSystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.etcd = {
          enable = true;
          endpoints = [ "http://etcd1.internal:2379" "http://etcd2.internal:2379" ];
          namespace = "/cluster/nixos";
          timeoutSeconds = 42;
        };
        ogygia.zookeeper = {
          enable = true;
          endpoints = [ "zk1.internal:2181" "zk2.internal:2181" ];
          namespace = "/cluster/nixos";
          timeoutSeconds = 42;
        };
      }
    ];
  };

  bothConfigPackage = bothSystem.config.ogygia.cliConfig.package or
    (throw ''Ogygia CLI config package not found. Ensure ogygia.cliConfig.enable is true when ogygia.enable = true.'');

in
pkgs.runCommand "ogygia-cli-config"
{
  nativeBuildInputs = [ pkgs.python3 ];
} ''
    # Test etcd only configuration
    etcdConf="${etcdOnlyConfigPackage}/share/ogygia/config.toml"
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${etcdOnlyConfigPackage}/share/ogygia/config.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "neb.test", og["domain"]
  etcd = og["etcd"]
  assert etcd["endpoints"] == ["http://etcd1.internal:2379", "http://etcd2.internal:2379"], etcd["endpoints"]
  assert etcd["namespace"] == "/cluster/nixos", etcd["namespace"]
  assert etcd["timeout_seconds"] == 42, etcd["timeout_seconds"]
  # ZooKeeper should not be present when only etcd is configured
  assert "zookeeper" not in og, "zookeeper should not be in config when only etcd is enabled"
  PY

    # Test zookeeper only configuration
    zkConf="${zkOnlyConfigPackage}/share/ogygia/config.toml"
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${zkOnlyConfigPackage}/share/ogygia/config.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "neb.test", og["domain"]
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.internal:2181", "zk2.internal:2181"], zk["endpoints"]
  assert zk["namespace"] == "/cluster/nixos", zk["namespace"]
  assert zk["timeout_seconds"] == 42, zk["timeout_seconds"]
  # etcd should not be present when only zookeeper is configured
  assert "etcd" not in og, "etcd should not be in config when only zookeeper is enabled"
  PY

    # Test both etcd and zookeeper configuration
    bothConf="${bothConfigPackage}/share/ogygia/config.toml"
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${bothConfigPackage}/share/ogygia/config.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "neb.test", og["domain"]
  # Both etcd and zookeeper should be present
  etcd = og["etcd"]
  assert etcd["endpoints"] == ["http://etcd1.internal:2379", "http://etcd2.internal:2379"], etcd["endpoints"]
  assert etcd["namespace"] == "/cluster/nixos", etcd["namespace"]
  assert etcd["timeout_seconds"] == 42, etcd["timeout_seconds"]
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.internal:2181", "zk2.internal:2181"], zk["endpoints"]
  assert zk["namespace"] == "/cluster/nixos", zk["namespace"]
  assert zk["timeout_seconds"] == 42, zk["timeout_seconds"]
  PY

    mkdir -p $out
    cp "$etcdConf" "$out/config-etcd.toml"
    cp "$zkConf" "$out/config-zk.toml"
    cp "$bothConf" "$out/config-both.toml"
''
