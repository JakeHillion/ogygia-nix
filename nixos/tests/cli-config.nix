{ pkgs, lib, system, ogygiaModule }:

let
  configTestSystem = lib.nixosSystem {
    inherit system;
    modules = [
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

  configPackage = configTestSystem.config.ogygia.cliConfig.package or
    (throw ''Ogygia CLI config package not found. Ensure ogygia.cliConfig.enable is true when ogygia.enable = true.'');

in
pkgs.runCommand "ogygia-cli-config"
{
  nativeBuildInputs = [ pkgs.python3 ];
} ''
    conf="${configPackage}/share/ogygia/config.toml"
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  from pathlib import Path
  conf = Path("${configPackage}/share/ogygia/config.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "neb.test", og["domain"]
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.internal:2181", "zk2.internal:2181"], zk["endpoints"]
  assert zk["namespace"] == "/cluster/nixos", zk["namespace"]
  assert zk["timeout_seconds"] == 42, zk["timeout_seconds"]
  PY
    mkdir -p $out
    cp "$conf" "$out/config.toml"
''
