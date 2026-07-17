{ config, lib, pkgs, ogygia-updated, ... }:

let
  cfg = config.ogygia.updated;

  tomlFormat = pkgs.formats.toml { };

  configFile = tomlFormat.generate "ogygia-updated.toml" cfg.settings;
in
{
  options.ogygia.updated = {
    enable = lib.mkEnableOption "the ogygia automatic update daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = ogygia-updated;
      description = "The ogygia-updated package to use.";
    };

    settings = lib.mkOption {
      type = lib.types.submodule {
        freeformType = tomlFormat.type;
      };
      default = { };
      description = ''
        Freeform configuration for ogygia-updated, serialized directly to
        TOML. Keys map 1:1 to the application config schema. Any key not
        set here uses the application's compiled-in default.

        Commonly used keys:

        - `repo.url` (string) — git remote to track. Defaults to
          `ogygia.gitRemoteUrl`.

        - `repo.branch` (string) — branch whose tip the host follows.
          Default: "main".

        - `host.name` (string) — attribute name of this host's
          nixosConfiguration in the flake. Defaults to the FQDN.

        - `build.prefetch_attr` (string) — a cache-resident edition of
          this host's closure (typically with the expensive
          commit-specific parts removed) substituted with `--max-jobs 0`
          before the real build. If it cannot be substituted the cycle is
          skipped rather than built locally, so cache-dependent hosts
          never fall back to a full build.

        - `activate.allow_reboot` (bool) — reboot automatically when an
          update changes the kernel. Default: false.

        - `activate.reboot_delay_minutes` (int) — minutes to wait before
          an automatic reboot. Default: 15.

        - `daemon.initial_delay_seconds` (int) — delay before the first
          update cycle after startup. Default: 900.

        - `daemon.interval_seconds` (int) — seconds between update
          cycles. Default: 3600.

        - `daemon.jitter_seconds` (int) — upper bound of the random
          extra delay added to each interval. Default: 1800.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    ogygia.updated.settings = {
      repo.url = lib.mkDefault config.ogygia.gitRemoteUrl;
      host.name = lib.mkDefault config.networking.fqdn;
    };

    systemd.services.ogygia-updated = {
      description = "Ogygia Automatic Update Daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      # The daemon activates new configurations, so switch-to-configuration
      # runs as its child and must never take it down mid-update. Instead
      # the daemon exits after activating and Restart brings it back under
      # the new unit definition.
      restartIfChanged = false;
      stopIfChanged = false;

      path = [ config.nix.package config.systemd.package ];

      serviceConfig = {
        ExecStart = "${cfg.package}/bin/ogygia-updated --config ${configFile}";
        Restart = "always";
        RestartSec = "5s";
        StateDirectory = "ogygia-updated";
        # Holds the control socket; root-only so filesystem permissions
        # authorize manual `ogygia update` triggers.
        RuntimeDirectory = "ogygia-updated";
        RuntimeDirectoryMode = "0700";
      };

      environment = {
        RUST_LOG = lib.mkDefault "info";
      };
    };
  };
}
