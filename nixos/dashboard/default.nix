{ config, lib, pkgs, ogygia-dashboard, ... }:

let
  cfg = config.ogygia.dashboard;
  etcdCfg = config.ogygia.etcd;

  ogygiaConfig = config.ogygia;

  tomlFormat = pkgs.formats.toml { };

  configFile = tomlFormat.generate "ogygia-dashboard.toml" ({
    server = cfg.serverConfig;
    git = {
      remote_url = ogygiaConfig.gitRemoteUrl;
    };
    etcd = {
      endpoints = etcdCfg.endpoints;
      prefix = etcdCfg.namespace + "/nixos/versions";
    };
    title = cfg.title;
  } // lib.optionalAttrs (cfg.hostnameStripSuffix != null) {
    hostname_strip_suffix = cfg.hostnameStripSuffix;
  });
in
{
  options.ogygia.dashboard = {
    enable = lib.mkEnableOption "ogygia-dashboard NixOS host status visualization webserver";

    package = lib.mkOption {
      type = lib.types.package;
      default = ogygia-dashboard;
      description = "The ogygia-dashboard package to use.";
    };

    title = lib.mkOption {
      type = lib.types.str;
      default = config.ogygia.domain;
      description = "Title displayed on the dashboard page. Defaults to the island domain.";
    };

    hostnameStripSuffix = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = ".${config.ogygia.domain}";
      description = "Suffix to strip from hostnames when displaying them on the dashboard. Defaults to the island domain.";
      example = ".example.com";
    };

    serverConfig = lib.mkOption {
      type = lib.types.attrs;
      default = { port = 8080; };
      description = "Server bind configuration passed to the dashboard TOML config.";
      example = { port = 8080; };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = etcdCfg.endpoints != [ ];
        message = "ogygia.etcd.endpoints must not be empty when ogygia.dashboard is enabled.";
      }
    ];

    systemd.services.ogygia-dashboard = {
      description = "Ogygia Dashboard - NixOS Host Status Visualization";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/ogygia-dashboard --config ${configFile}";
        Restart = "always";
        RestartSec = "10s";
        DynamicUser = true;
        RuntimeDirectory = "ogygia-dashboard";

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
      };

      environment = {
        RUST_LOG = lib.mkDefault "info";
      };
    };
  };
}
