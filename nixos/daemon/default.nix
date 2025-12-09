{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.daemon;
in
{
  options.ogygia.daemon = {
    enable = lib.mkEnableOption "ogygiad daemon for publishing system state to ZooKeeper";

    zookeeper = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable ZooKeeper publishing";
      };
    };
  };

  config = lib.mkIf (cfg.enable && cfg.zookeeper.enable) {
    # Ensure ZooKeeper configuration is enabled
    assertions = [
      {
        assertion = config.ogygia.zookeeper.enable;
        message = "ogygia.daemon requires ogygia.zookeeper to be enabled";
      }
    ];

    systemd.services.ogygiad = {
      description = "Ogygia daemon for publishing system state to ZooKeeper";
      documentation = [ "https://github.com/user/ogygia" ];

      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        Type = "simple";
        ExecStart = "${pkgs.ogygiad}/bin/ogygiad";
        Restart = "on-failure";
        RestartSec = "10s";

        # Security hardening
        DynamicUser = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;

        # Allow reading system state paths
        ReadOnlyPaths = [
          "/run/current-system"
          "/run/booted-system"
          "/nix/var/nix/profiles/system"
        ];

        # Logging
        StandardOutput = "journal";
        StandardError = "journal";
        SyslogIdentifier = "ogygiad";

        # Resource limits
        MemoryMax = "256M";
        TasksMax = 64;
      };

      # Environment variables can be set here if needed
      environment = {
        RUST_LOG = lib.mkDefault "info";
      };
    };
  };
}
