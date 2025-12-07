{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  cliCfg = cfg.cliConfig;
  zkCfg = cfg.zookeeper;

  tomlFormat = pkgs.formats.toml { };

  configData =
    {
      ogygia =
        { domain = cfg.domain; } //
        (lib.optionalAttrs zkCfg.enable {
          zookeeper = {
            endpoints = zkCfg.endpoints;
            namespace = zkCfg.namespace;
            timeout_seconds = zkCfg.timeoutSeconds;
          };
        });
    };

  generatedToml = tomlFormat.generate "config.toml" configData;

  configPackage = pkgs.runCommand "share-ogygia-config" { } ''
    mkdir -p $out/share/ogygia
    cp ${generatedToml} $out/share/ogygia/config.toml
  '';
in
{
  options.ogygia.cliConfig = {
    enable = lib.mkEnableOption "render shared configuration for the Ogygia CLI and tooling";

    package = lib.mkOption {
      internal = true;
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Derivation containing the generated Ogygia CLI configuration.";
    };
  };

  options.ogygia.zookeeper = {
    enable = lib.mkEnableOption "ZooKeeper configuration rendering for Ogygia tooling";

    endpoints = lib.mkOption {
      type = with lib.types; listOf str;
      default = [ ];
      example = [ "zk-internal-1:2181" "zk-internal-2:2181" ];
      description = ''
        List of ZooKeeper endpoints in <host>:<port> form that publish host state
        information. The CLI uses these endpoints to read global status data.
      '';
    };

    namespace = lib.mkOption {
      type = lib.types.str;
      default = "/nixos/versions";
      description = "ZooKeeper znode prefix that contains host state information.";
    };

    timeoutSeconds = lib.mkOption {
      type = lib.types.int;
      default = 10;
      description = "Connection timeout used by the Ogygia CLI when contacting ZooKeeper.";
    };
  };

  config = lib.mkIf (cfg.enable && cliCfg.enable) {
    assertions = lib.optionals zkCfg.enable [
      {
        assertion = zkCfg.endpoints != [ ];
        message = "ogygia.zookeeper.endpoints must not be empty when ZooKeeper integration is enabled.";
      }
    ];

    ogygia.cliConfig.package = configPackage;

    environment = {
      systemPackages = [ configPackage ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
