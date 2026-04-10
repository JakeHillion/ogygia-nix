{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  etcdCfg = cfg.etcd;
in
{
  options.ogygia.etcd = {
    enable = lib.mkEnableOption "etcd configuration rendering for Ogygia tooling";

    endpoints = lib.mkOption {
      type = with lib.types; listOf str;
      default = [ ];
      example = [ "http://etcd-internal-1:2379" "http://etcd-internal-2:2379" ];
      description = ''
        List of etcd endpoints in URL form that publish host state
        information. The CLI uses these endpoints to read global status data.
      '';
    };

    namespace = lib.mkOption {
      type = lib.types.str;
      default = "/ogygia/nixos/versions";
      description = "etcd key prefix that contains host state information.";
    };

    timeoutSeconds = lib.mkOption {
      type = lib.types.int;
      default = 10;
      description = "Connection timeout used by the Ogygia CLI when contacting etcd.";
    };
  };

  config = lib.mkIf (cfg.enable && cfg.cliConfig.enable && etcdCfg.enable) {
    assertions = [
      {
        assertion = etcdCfg.endpoints != [ ];
        message = "ogygia.etcd.endpoints must not be empty when etcd integration is enabled.";
      }
    ];
  };
}
