{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  zkCfg = cfg.zookeeper;
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

        :::{.warning}
        ZooKeeper support is deprecated and will be removed in a future version.
        Please migrate to etcd, which is more actively supported and will receive all future features.
        :::
      '';
    };

    namespace = lib.mkOption {
      type = lib.types.str;
      default = "/nixos/versions";
      description = ''
        ZooKeeper znode prefix that contains host state information.

        :::{.warning}
        ZooKeeper support is deprecated and will be removed in a future version.
        Please migrate to etcd, which is more actively supported and will receive all future features.
        :::
      '';
    };

    timeoutSeconds = lib.mkOption {
      type = lib.types.int;
      default = 10;
      description = ''
        Connection timeout used by the Ogygia CLI when contacting ZooKeeper.

        :::{.warning}
        ZooKeeper support is deprecated and will be removed in a future version.
        Please migrate to etcd, which is more actively supported and will receive all future features.
        :::
      '';
    };
  };

  config = lib.mkIf (cfg.enable && cfg.cliConfig.enable && zkCfg.enable)
    (lib.warn
      "ogygia: ZooKeeper support is deprecated and will be removed in a future version. Please migrate to etcd, which is more actively supported and will receive all future features."
      {
        assertions = lib.optionals zkCfg.enable [
          {
            assertion = zkCfg.endpoints != [ ];
            message = "ogygia.zookeeper.endpoints must not be empty when ZooKeeper integration is enabled.";
          }
        ];
      });
}
