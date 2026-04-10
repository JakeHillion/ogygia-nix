{ config, lib, pkgs, ogygia-irisd, ... }:

let
  cfg = config.ogygia;
  islands = cfg._internalIslands;

  tomlFormat = pkgs.formats.toml { };

  # Generate irisd config and service for a single island
  mkIslandIrisd = islandName: islandCfg:
    let
      # Use the first listen address for the local Nix substituter URL
      substituterUrl = if islandCfg.irisd.settings.server.listen != [ ]
        then "http://${builtins.head islandCfg.irisd.settings.server.listen}"
        else throw "ogygia.islands.${islandName}.irisd.settings.server.listen must not be empty";

      configFile = tomlFormat.generate "ogygia-irisd-${islandName}.toml" islandCfg.irisd.settings;
    in
    {
      # Return systemd service configuration
      systemdService = {
        description = "Ogygia Peer-to-Peer Nix Binary Cache (${islandName} island)";
        wantedBy = [ "multi-user.target" ];
        after = [ "network.target" "nix-daemon.socket" ];
        requires = [ "nix-daemon.socket" ];

        path = [ config.nix.package ];

        serviceConfig = {
          ExecStart = "${islandCfg.irisd.package}/bin/ogygia-irisd --config ${configFile}";
          Restart = "always";
          RestartSec = "60s";
          DynamicUser = true;
          RuntimeDirectory = "ogygia-irisd-${islandName}";

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

          # Read access to /nix/store for on-demand NAR generation
          # and to nix daemon socket for path-info queries
          ReadOnlyPaths = [ "/nix/store" "/nix/var/nix/daemon-socket" ];
        };

        environment = {
          RUST_LOG = lib.mkDefault "info";
          # Enable nix-command feature for nix path-info queries
          NIX_CONFIG = "experimental-features = nix-command";
          # Set island name for potential use by the application
          OGYGIA_ISLAND = islandName;
        };
      };

      # Return Nix substituter URL if configureNixDaemon is enabled
      nixSubstituter = lib.mkIf islandCfg.irisd.configureNixDaemon substituterUrl;

      # Assertions for this island
      assertions = [
        {
          assertion = islandCfg.irisd.settings.server.listen != [ ];
          message = "ogygia.islands.${islandName}.irisd.settings.server.listen must not be empty.";
        }
      ];
    };

  # Collect all island irisd configurations
  islandIrisdConfigs = lib.mapAttrs mkIslandIrisd islands;

  # Filter to only enabled islands with irisd enabled
  enabledIslands = lib.filterAttrs (name: islandCfg: islandCfg.enable && islandCfg.irisd.enable) islands;

  # Generate all systemd services
  # For backwards compatibility: if only the "default" island exists (from legacy config),
  # use the old service name "ogygia-irisd.service"
  islandServices = lib.mapAttrs'
    (islandName: islandCfg: 
      let
        serviceName = if islandName == "default" && lib.length (lib.attrNames enabledIslands) == 1
          then "ogygia-irisd"
          else "ogygia-irisd-${islandName}";
      in
      lib.nameValuePair serviceName (mkIslandIrisd islandName islandCfg).systemdService
    )
    enabledIslands;

  # Collect all Nix substituters (maintain order)
  islandSubstituters = lib.concatLists (
    lib.mapAttrsToList
      (islandName: islandCfg:
        lib.optional islandCfg.irisd.configureNixDaemon
          "http://${builtins.head islandCfg.irisd.settings.server.listen}"
      )
      enabledIslands
  );

  # Collect all assertions
  allAssertions = lib.concatLists (
    lib.mapAttrsToList
      (islandName: islandCfg: (mkIslandIrisd islandName islandCfg).assertions)
      enabledIslands
  );
in
{
  config = lib.mkIf (enabledIslands != {}) {
    assertions = allAssertions;

    # Create systemd services for each enabled island
    systemd.services = islandServices;

    # Configure Nix daemon with all substituters (in order)
    nix.settings.substituters = lib.mkIf (islandSubstituters != [ ]) (lib.mkBefore islandSubstituters);
  };
}
