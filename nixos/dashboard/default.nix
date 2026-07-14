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
    } // lib.optionalAttrs cfg.ssh.enable {
      ssh = {
        url = cfg.ssh.url;
        key_path = "/run/credentials/ogygia-dashboard.service/ssh-key";
      } // lib.optionalAttrs (cfg.ssh.hostKey != null) {
        host_key = cfg.ssh.hostKey;
      };
    } // lib.optionalAttrs cfg.archive.enable {
      archive = {
        branch = cfg.archive.branch;
      };
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

    ssh = {
      enable = lib.mkEnableOption "SSH transport for all git operations, allowing private repositories and pushing";

      url = lib.mkOption {
        type = lib.types.str;
        description = "SSH URL for the repository at ogygia.gitRemoteUrl.";
        example = "git@git.example.com:user/repo.git";
      };

      keyFile = lib.mkOption {
        type = lib.types.str;
        description = "Path to an SSH private key, e.g. a Gitea deploy key. Loaded via systemd LoadCredential. Needs write access when archive is enabled.";
      };

      hostKey = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Expected SSH host key of the server in known_hosts format. Any host key is accepted when null.";
        example = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI...";
      };
    };

    archive = {
      enable = lib.mkEnableOption "archiving deployed commits to a persistent branch so they survive force-pushes";

      branch = lib.mkOption {
        type = lib.types.str;
        default = "ogygia/deployed-commits-archive";
        description = "Branch the deployed commits are archived to.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = etcdCfg.endpoints != [ ];
        message = "ogygia.etcd.endpoints must not be empty when ogygia.dashboard is enabled.";
      }
      {
        assertion = cfg.archive.enable -> cfg.ssh.enable;
        message = "ogygia.dashboard.archive requires ogygia.dashboard.ssh for pushing.";
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
      } // lib.optionalAttrs cfg.ssh.enable {
        LoadCredential = [ "ssh-key:${cfg.ssh.keyFile}" ];
      };

      environment = {
        RUST_LOG = lib.mkDefault "info";
      } // lib.optionalAttrs cfg.ssh.enable {
        # libgit2's SSH transport resolves $HOME/.ssh/known_hosts before the
        # host key callback and fails outright ("error loading known_hosts")
        # when HOME is unset, which it is under DynamicUser.
        HOME = "/run/ogygia-dashboard";
      };
    };
  };
}
