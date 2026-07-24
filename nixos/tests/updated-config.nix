{ pkgs, lib, ogygiaModule }:

let
  system = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.gitRemoteUrl = "https://git.neb.test/user/nixos.git";
        ogygia.updated.enable = true;
        networking.hostName = "host";
        networking.domain = "neb.test";
      }
    ];
  };

  etcEntry = system.config.environment.etc."ogygia/updated.toml";
  execStart = system.config.systemd.services.ogygia-updated.serviceConfig.ExecStart;

  # `ogygia update canary` locates this file through a constant compiled into
  # the CLI. Nothing at runtime reconciles the two, and a mismatch degrades
  # silently to no branch candidates, so pin them together here.
  cliDefaultPath = builtins.head (builtins.match
    ".*DEFAULT_CONFIG_PATH: &str = \"([^\"]+)\";.*"
    (builtins.readFile ../../src/ogygia-updated/src/config.rs));
in
pkgs.runCommand "ogygia-updated-config"
{
  nativeBuildInputs = [ pkgs.python3 ];
} ''
  ${lib.optionalString (cliDefaultPath != "/etc/ogygia/updated.toml") (throw ''
    The CLI's DEFAULT_CONFIG_PATH is "${cliDefaultPath}" but the module renders
    the daemon config to /etc/ogygia/updated.toml. Branch completion for
    `ogygia update canary` reads the CLI's path, so these must agree.
  '')}

  # The daemon and the CLI must read the same file, not two copies that can
  # drift; the unit is what proves the daemon uses the well-known path.
  case "${execStart}" in
    *" --config ${cliDefaultPath}") ;;
    *) echo "ExecStart does not use ${cliDefaultPath}: ${execStart}" >&2; exit 1 ;;
  esac

  python3 - <<'PY'
  import tomllib
  from pathlib import Path

  data = tomllib.loads(Path("${etcEntry.source}").read_text())
  assert data["repo"]["url"] == "https://git.neb.test/user/nixos.git", data
  assert data["host"]["name"] == "host.neb.test", data
  PY

  mkdir -p $out
  cp "${etcEntry.source}" "$out/updated.toml"
''
