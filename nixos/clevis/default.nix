{ config, lib, pkgs, ogygia-clevis, ... }:

let
  cfg = config.ogygia.clevis;

  jsonFormat = pkgs.formats.json { };

  configFile = jsonFormat.generate "ogygia-clevis.json" {
    secret_file = cfg.secretFile;
    inherit (cfg) spec;
  };
in
{
  options.ogygia.clevis = {
    enable = lib.mkEnableOption "keeping a Clevis SSS blob bound to the reachable Tang servers in its spec";

    package = lib.mkOption {
      type = lib.types.package;
      default = ogygia-clevis;
      description = "The ogygia-clevis package to use.";
    };

    secretFile = lib.mkOption {
      type = lib.types.str;
      example = "/data/disk_encryption.jwe";
      description = ''
        The Clevis JWE to maintain, typically the `secretFile` of a
        `boot.initrd.clevis` device. It must already exist and be
        decryptable from this host; it is replaced atomically whenever
        binding to the currently reachable Tang servers would cover more
        of the spec than it does now.
      '';
    };

    spec = lib.mkOption {
      type = jsonFormat.type;
      example = {
        t = 1;
        pins.tang = [
          { url = "http://tang1.example.com:7654"; thp = "H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE"; }
          { url = "http://tang2.example.com:7654"; thp = "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms"; }
        ];
      };
      description = ''
        The configuration to bind against, as it would be passed to
        `clevis encrypt sss`, except that every Tang pin must carry the
        `thp` of the server's signing key (as printed by `tang-show-keys`)
        so no server is ever trusted on first use. Rotate a server's keys
        by updating its `thp` here.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.etc."ogygia/clevis.json".source = configFile;

    systemd.services.ogygia-clevis = {
      description = "Ogygia Clevis Blob Maintenance";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      # The clevis wrapper brings jose along but not the curl its tang pin
      # shells out to.
      path = [ pkgs.clevis pkgs.curl ];

      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/ogygia-clevis --config /etc/ogygia/clevis.json";

        # Security hardening. Runs as root because the blob is root-only.
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

        ReadWritePaths = [ (dirOf cfg.secretFile) ];
      };

      environment = {
        RUST_LOG = lib.mkDefault "info";
      };
    };

    systemd.timers.ogygia-clevis = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "5m";
        OnUnitActiveSec = "1h";
        RandomizedDelaySec = "10m";
      };
    };
  };
}
