{ pkgs, lib, ogygiaModule }:

let
  # Test 1: Legacy config (backwards compatibility)
  legacyTestSystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "legacy.test";
        ogygia.zookeeper = {
          enable = true;
          endpoints = [ "zk1.legacy:2181" "zk2.legacy:2181" ];
          namespace = "/cluster/legacy";
          timeoutSeconds = 30;
        };
      }
    ];
  };

  # Test 2: New islands config
  islandsTestSystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.islands."primary" = {
          enable = true;
          domain = "primary.test";
          zookeeper = {
            enable = true;
            endpoints = [ "zk1.primary:2181" ];
            namespace = "/cluster/primary";
            timeoutSeconds = 15;
          };
        };

        ogygia.islands."secondary" = {
          enable = true;
          domain = "secondary.test";
          zookeeper = {
            enable = true;
            endpoints = [ "zk1.secondary:2181" ];
            namespace = "/cluster/secondary";
          };
        };
      }
    ];
  };

  # Test 3: Mixed config (legacy + explicit islands)
  mixedTestSystem = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        # Legacy config - will be converted to "default" island
        ogygia.enable = true;
        ogygia.domain = "mixed-legacy.test";

        # Explicit island named "extra"
        ogygia.islands."extra" = {
          domain = "mixed-extra.test";
        };
      }
    ];
  };

  legacyConfigPackage = legacyTestSystem.config.ogygia._internalIslands.default.cliConfig.package or
    (throw ''Legacy island config package not found'');

  primaryConfigPackage = islandsTestSystem.config.ogygia._internalIslands.primary.cliConfig.package or
    (throw ''Primary island config package not found'');

  secondaryConfigPackage = islandsTestSystem.config.ogygia._internalIslands.secondary.cliConfig.package or
    (throw ''Secondary island config package not found'');

  defaultMixedPackage = mixedTestSystem.config.ogygia._internalIslands.default.cliConfig.package or
    (throw ''Default island (from legacy) config package not found'');

  extraMixedPackage = mixedTestSystem.config.ogygia._internalIslands.extra.cliConfig.package or
    (throw ''Extra island config package not found'');

in
pkgs.runCommand "ogygia-cli-config"
{
  nativeBuildInputs = [ pkgs.python3 ];
} ''
    # Test legacy config
    echo "Testing legacy config..."
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${legacyConfigPackage}/share/ogygia/config-legacy.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "legacy.test", f"Expected 'legacy.test', got {og['domain']}"
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.legacy:2181", "zk2.legacy:2181"], f"Endpoints mismatch: {zk['endpoints']}"
  assert zk["namespace"] == "/cluster/legacy", f"Namespace mismatch: {zk['namespace']}"
  assert zk["timeout_seconds"] == 30, f"Timeout mismatch: {zk['timeout_seconds']}"
  print("✓ Legacy config test passed")
  PY

    # Test primary island config
    echo "Testing primary island config..."
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${primaryConfigPackage}/share/ogygia/config-primary.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "primary.test", f"Expected 'primary.test', got {og['domain']}"
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.primary:2181"], f"Endpoints mismatch: {zk['endpoints']}"
  assert zk["namespace"] == "/cluster/primary", f"Namespace mismatch: {zk['namespace']}"
  assert zk["timeout_seconds"] == 15, f"Timeout mismatch: {zk['timeout_seconds']}"
  print("✓ Primary island config test passed")
  PY

    # Test secondary island config
    echo "Testing secondary island config..."
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${secondaryConfigPackage}/share/ogygia/config-secondary.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "secondary.test", f"Expected 'secondary.test', got {og['domain']}"
  zk = og["zookeeper"]
  assert zk["endpoints"] == ["zk1.secondary:2181"], f"Endpoints mismatch: {zk['endpoints']}"
  assert zk["namespace"] == "/cluster/secondary", f"Namespace mismatch: {zk['namespace']}"
  assert zk["timeout_seconds"] == 10, f"Timeout mismatch: {zk['timeout_seconds']}"
  print("✓ Secondary island config test passed")
  PY

    # Test mixed config - default island (from legacy)
    echo "Testing mixed config (default island from legacy)..."
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${defaultMixedPackage}/share/ogygia/config-default.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "mixed-legacy.test", f"Expected 'mixed-legacy.test', got {og['domain']}"
  print("✓ Mixed config default island test passed")
  PY

    # Test mixed config - extra island (explicit)
    echo "Testing mixed config (explicit extra island)..."
    python3 - <<'PY'
  import tomllib
  from pathlib import Path
  conf = Path("${extraMixedPackage}/share/ogygia/config-extra.toml")
  data = tomllib.loads(conf.read_text())
  og = data["ogygia"]
  assert og["domain"] == "mixed-extra.test", f"Expected 'mixed-extra.test', got {og['domain']}"
  print("✓ Mixed config extra island test passed")
  PY

    mkdir -p $out
    echo "All tests passed!" > $out/result
''
