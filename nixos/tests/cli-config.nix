{ pkgs, lib, ogygiaModule }:

let
  # Test etcd only configuration (using new pattern without deprecated enable)
  etcdOnlySystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.etcd = {
          endpoints = [ "http://etcd1.internal:2379" "http://etcd2.internal:2379" ];
          namespace = "/cluster/nixos";
          timeoutSeconds = 42;
        };
      }
    ];
  };

  etcdOnlyConfigPackage = etcdOnlySystem.config.ogygia.cliConfig.package or
    (throw ''Ogygia CLI config package not found. Ensure ogygia.cliConfig.enable is true when ogygia.enable = true.'');

  # Test deprecation warning with legacy enable flag
  etcdWithDeprecatedEnable = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.etcd = {
          enable = true;
          endpoints = [ "http://etcd1.internal:2379" ];
        };
      }
    ];
  };

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

    # Test deprecation warning is emitted when enable is used
    deprecatedConf="${etcdWithDeprecatedEnable.config.ogygia.cliConfig.package}/share/ogygia/config.toml"
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${etcdWithDeprecatedEnable.config.ogygia.cliConfig.package}/share/ogygia/config.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  # Config should still work with deprecated enable flag
  etcd = og["etcd"]
  assert etcd["endpoints"] == ["http://etcd1.internal:2379"], etcd["endpoints"]
  PY

    # Verify deprecation warning is emitted
    ${lib.concatMapStringsSep "\n    " (w: "echo 'Warning:' ${lib.escapeShellArg w}") etcdWithDeprecatedEnable.config.warnings}

    mkdir -p $out
    cp "$etcdConf" "$out/config-etcd.toml"
''
