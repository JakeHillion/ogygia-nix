{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  etcdCfg = cfg.etcd;
  etcdEnabled = etcdCfg.endpoints != [ ];
in
{
  options.ogygia.etcd = {
    enable = lib.mkOption {
      type = lib.types.nullOr lib.types.bool;
      default = null;
      description = ''
        DEPRECATED: This option is deprecated and will be removed in a future release.
        etcd integration is now automatically enabled when endpoints are provided.

        Previously enabled etcd configuration rendering for Ogygia tooling.
        This option no longer has any effect - endpoints alone determine enablement.
      '';
    };

    endpoints = lib.mkOption {
      type = with lib.types; listOf str;
      default = [ ];
      example = [ "http://etcd-internal-1:2379" "http://etcd-internal-2:2379" ];
      description = ''
        List of etcd endpoints in URL form that publish host state
        information. The CLI uses these endpoints to read global status data.
        etcd integration is automatically enabled when this list is non-empty.
      '';
    };

    namespace = lib.mkOption {
      type = lib.types.str;
      default = "/ogygia";
      description = "etcd key prefix that contains host state information.";
    };

    timeoutSeconds = lib.mkOption {
      type = lib.types.int;
      default = 10;
      description = "Connection timeout used by the Ogygia CLI when contacting etcd.";
    };

    clientConnectionString = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      default = lib.concatStringsSep "," etcdCfg.endpoints;
      description = ''
        Read-only option that generates a comma-separated connection string
        from the configured endpoints. This is used by services like
        ogygia-hostinfod to connect to etcd.
      '';
    };
  };

  config = lib.mkMerge [
    {
      warnings = lib.optionals (etcdCfg.enable != null) [
        "ogygia.etcd.enable is deprecated and will be removed in a future release. etcd integration is now automatically enabled when endpoints are provided. Remove the 'enable' option from your configuration."
      ];
    }
    (lib.mkIf (cfg.enable && cfg.cliConfig.enable && etcdEnabled) {
      assertions = [
        {
          # etcdEnabled already checks endpoints != [], so this assertion
          # is effectively redundant now but kept for clarity
          assertion = etcdEnabled;
          message = "ogygia.etcd.endpoints must not be empty when etcd integration is enabled.";
        }
      ];
    })
  ];
}
