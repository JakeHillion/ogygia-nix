{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.ogygiad;
  ogyCfg = config.ogygia;

  configFile = pkgs.writeText "ogygiad.toml" (lib.generators.toTOML { } {
    zookeeper = {
      addresses = cfg.zookeeper.addresses;
      enable_version_upload = cfg.zookeeper.enableVersionUpload;
      hostname = cfg.zookeeper.hostname;
    };
  });
in
{
  options.ogygia.ogygiad = {
    enable = lib.mkEnableOption "ogygiad daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.ogygiad or (throw "ogygiad package not available");
      description = "The ogygiad package to use";
    };

    zookeeper = {
      addresses = lib.mkOption {
        type = lib.types.str;
        description = "ZooKeeper server addresses (comma-separated host:port pairs)";
        example = "zk1.example.com:2181,zk2.example.com:2181,zk3.example.com:2181";
      };

      enableVersionUpload = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable uploading system version information to ZooKeeper";
      };

      hostname = lib.mkOption {
        type = lib.types.str;
        default = config.networking.fqdn;
        defaultText = lib.literalExpression "config.networking.fqdn";
        description = "Hostname to use in ZooKeeper paths (typically FQDN)";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.ogygiad = {
      description = "Ogygia daemon for managing island infrastructure";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/ogygiad ${configFile}";
        Restart = "always";
        RestartSec = "10s";
        DynamicUser = true;

        # Security hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadOnlyPaths = [
          "/run/current-system"
          "/run/booted-system"
          "/nix/var/nix/profiles/system"
        ];
      };
    };
  };
}
