{ config, lib, pkgs, ogygia-hostinfod, ... }:

let
  cfg = config.ogygia.hostinfod;
  etcdCfg = config.ogygia.etcd;
in
{
  options.ogygia.hostinfod = {
    enable = lib.mkEnableOption "ogygia-hostinfod etcd publisher for host version tracking";

    package = lib.mkOption {
      type = lib.types.package;
      default = ogygia-hostinfod;
      description = "The ogygia-hostinfod package to use.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.ogygia-hostinfod = {
      description = "Ogygia Host Info etcd Publisher";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/ogygia-hostinfod --endpoints ${lib.escapeShellArg etcdCfg.clientConnectionString} --prefix ${lib.escapeShellArg (etcdCfg.namespace + "/nixos/versions")}";
        Restart = "always";
        RestartSec = "10s";
        DynamicUser = true;
        RuntimeDirectory = "ogygia-hostinfod";

        # Security hardening
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;

        # Read access to system directories for version tracking
        # Note: We need access to parent directories because inotify watches
        # the directory containing the symlink, not the symlink itself
        ReadOnlyPaths = [
          "/run"
          "/nix/var/nix/profiles"
        ];
      };

      environment = {
        RUST_LOG = lib.mkDefault "info";
      };
    };
  };
}
