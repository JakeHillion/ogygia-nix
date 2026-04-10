{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;

  tomlFormat = pkgs.formats.toml { };

  # Check if legacy options are being used (non-default values set)
  hasLegacyConfig = cfg.enable || cfg.domain != null || cfg.nebula.ipv4 != null 
    || cfg.zookeeper.enable || cfg.irisd.enable || cfg.versions.enable;

  # Create a "default" island from legacy config if legacy options are used
  legacyIslandConfig = {
    enable = true;
    domain = cfg.domain;
    nebula.ipv4 = cfg.nebula.ipv4;
    zookeeper = cfg.zookeeper;
    irisd = cfg.irisd;
    cliConfig.enable = cfg.cliConfig.enable;
    versions.enable = cfg.versions.enable;
    versions.build_revision.enable = cfg.versions.build_revision.enable;
  };

  # Merge legacy island with explicit islands (explicit takes precedence for 'default' name)
  allIslands = if hasLegacyConfig
    then lib.recursiveUpdate { default = legacyIslandConfig; } cfg.islands
    else cfg.islands;

  # Define the island submodule type
  islandModule = { name, config, ... }: {
    options = {
      enable = lib.mkEnableOption "this island" // {
        default = true;
      };

      domain = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Domain name for this island";
        example = "island.example.com";
      };

      nebula.ipv4 = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "This node's Nebula IPv4 address for internal P2P communication.";
        example = "172.20.0.1";
      };

      zookeeper = lib.mkOption {
        type = lib.types.submodule {
          options = {
            enable = lib.mkEnableOption "ZooKeeper configuration rendering for this island";
            endpoints = lib.mkOption {
              type = with lib.types; listOf str;
              default = [ ];
              example = [ "zk-internal-1:2181" "zk-internal-2:2181" ];
              description = "List of ZooKeeper endpoints in <host>:<port> form.";
            };
            namespace = lib.mkOption {
              type = lib.types.str;
              default = "/nixos/versions";
              description = "ZooKeeper znode prefix that contains host state information.";
            };
            timeoutSeconds = lib.mkOption {
              type = lib.types.int;
              default = 10;
              description = "Connection timeout used by the Ogygia CLI when contacting ZooKeeper.";
            };
          };
        };
        default = {};
        description = "ZooKeeper configuration for this island.";
      };

      cliConfig = lib.mkOption {
        type = lib.types.submodule {
          options = {
            enable = lib.mkEnableOption "render shared configuration for the Ogygia CLI" // {
              default = true;
            };
            package = lib.mkOption {
              internal = true;
              type = lib.types.nullOr lib.types.package;
              default = null;
              description = "Derivation containing the generated Ogygia CLI configuration.";
            };
          };
        };
        default = {};
        description = "CLI configuration for this island.";
      };

      irisd = lib.mkOption {
        type = lib.types.submodule {
          freeformType = tomlFormat.type;
          options = {
            enable = lib.mkEnableOption "ogygia-irisd peer-to-peer Nix binary cache for this island";
            package = lib.mkOption {
              type = lib.types.package;
              default = pkgs.ogygia-irisd or (throw "ogygia-irisd package not available - ensure ogygia-irisd is in pkgs");
              description = "The ogygia-irisd package to use.";
            };
            settings = lib.mkOption {
              type = lib.types.submodule {
                freeformType = tomlFormat.type;
                options = {
                  server.listen = lib.mkOption {
                    type = lib.types.listOf lib.types.str;
                    default = [ ];
                    description = "HTTP listen addresses for the binary cache.";
                  };
                };
              };
              default = { };
              description = "Freeform configuration for ogygia-irisd, serialized directly to TOML.";
            };
            configureNixDaemon = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Configure the local Nix daemon to use irisd as a preferred substituter.";
            };
          };
        };
        default = {};
        description = "Irisd configuration for this island.";
      };

      versions = lib.mkOption {
        type = lib.types.submodule {
          options = {
            enable = lib.mkEnableOption "tooling for tracking versions within this island" // {
              default = true;
            };
            build_revision = lib.mkOption {
              type = lib.types.submodule {
                options = {
                  enable = lib.mkEnableOption "embedding the build revision in the system closure for this island" // {
                    default = true;
                  };
                };
              };
              default = {};
              description = "Build revision configuration for this island.";
            };
          };
        };
        default = {};
        description = "Version tracking configuration for this island.";
      };
    };

    config = lib.mkIf config.enable {
      # Set default listen addresses based on nebula.ipv4 if not already set
      irisd.settings.server.listen = lib.mkDefault (
        [ "127.0.0.1:35742" ]
        ++ lib.optionals (config.nebula.ipv4 != null) [
          "${config.nebula.ipv4}:35742"
        ]
      );
    };
  };

  # Generate a config file for a single island
  mkIslandConfig = islandName: islandCfg:
    let
      configData = lib.optionalAttrs (islandCfg.domain != null) {
        ogygia = {
          domain = islandCfg.domain;
        } // (lib.optionalAttrs islandCfg.zookeeper.enable {
          zookeeper = {
            endpoints = islandCfg.zookeeper.endpoints;
            namespace = islandCfg.zookeeper.namespace;
            timeout_seconds = islandCfg.zookeeper.timeoutSeconds;
          };
        });
      };

      generatedToml = tomlFormat.generate "config-${islandName}.toml" configData;
    in
    pkgs.runCommand "share-ogygia-config-${islandName}" { } ''
      mkdir -p $out/share/ogygia
      cp ${generatedToml} $out/share/ogygia/config-${islandName}.toml
    '';

  # Generate assertions for each island
  islandConfigAssertions = lib.concatMap
    (islandName:
      let islandCfg = allIslands.${islandName}; in
      lib.optionals (islandCfg.enable && islandCfg.zookeeper.enable) [
        {
          assertion = islandCfg.zookeeper.endpoints != [ ];
          message = "ogygia.islands.${islandName}.zookeeper.endpoints must not be empty when ZooKeeper integration is enabled.";
        }
      ]
    )
    (lib.attrNames allIslands);
in
{
  imports = [
    ./legacy-warnings.nix
    ./island-irisd.nix
    ./island-versions.nix
    # Note: Old modules (./config, ./irisd, ./versions) are no longer imported here.
    # Their functionality is now integrated into this module and the island-* modules.
    # The legacy options below provide backwards compatibility.
  ];

  options.ogygia = {
    # Legacy options (deprecated but functional)
    enable = lib.mkEnableOption "ogygia" // {
      visible = false;
    };

    domain = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      visible = false;
      description = "Domain name (deprecated: use ogygia.islands.<name>.domain)";
    };

    nebula.ipv4 = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      visible = false;
      description = "Nebula IPv4 address (deprecated: use ogygia.islands.<name>.nebula.ipv4)";
    };

    zookeeper = lib.mkOption {
      type = lib.types.submodule {
        options = {
          enable = lib.mkEnableOption "ZooKeeper (deprecated: use ogygia.islands.<name>.zookeeper)" // {
            visible = false;
          };
          endpoints = lib.mkOption {
            type = with lib.types; listOf str;
            default = [ ];
            visible = false;
          };
          namespace = lib.mkOption {
            type = lib.types.str;
            default = "/nixos/versions";
            visible = false;
          };
          timeoutSeconds = lib.mkOption {
            type = lib.types.int;
            default = 10;
            visible = false;
          };
        };
      };
      default = {};
      visible = false;
    };

    cliConfig = lib.mkOption {
      type = lib.types.submodule {
        options = {
          enable = lib.mkEnableOption "CLI config (deprecated: use ogygia.islands.<name>.cliConfig)" // {
            visible = false;
          };
          package = lib.mkOption {
            internal = true;
            type = lib.types.nullOr lib.types.package;
            default = null;
            visible = false;
          };
        };
      };
      default = {};
      visible = false;
    };

    irisd = lib.mkOption {
      type = lib.types.submodule {
        freeformType = tomlFormat.type;
        options = {
          enable = lib.mkEnableOption "irisd (deprecated: use ogygia.islands.<name>.irisd)" // {
            visible = false;
          };
          package = lib.mkOption {
            type = lib.types.package;
            default = pkgs.ogygia-irisd or (throw "ogygia-irisd package not available");
            visible = false;
          };
          settings = lib.mkOption {
            type = lib.types.submodule {
              freeformType = tomlFormat.type;
              options = {
                server.listen = lib.mkOption {
                  type = lib.types.listOf lib.types.str;
                  default = [ ];
                  visible = false;
                };
              };
            };
            default = { };
            visible = false;
          };
          configureNixDaemon = lib.mkOption {
            type = lib.types.bool;
            default = false;
            visible = false;
          };
        };
      };
      default = {};
      visible = false;
    };

    versions = lib.mkOption {
      type = lib.types.submodule {
        options = {
          enable = lib.mkEnableOption "versions (deprecated: use ogygia.islands.<name>.versions)" // {
            visible = false;
          };
          build_revision = lib.mkOption {
            type = lib.types.submodule {
              options = {
                enable = lib.mkEnableOption "build revision (deprecated)" // {
                  visible = false;
                };
              };
            };
            default = {};
            visible = false;
          };
        };
      };
      default = {};
      visible = false;
    };

    # New islands option
    islands = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule islandModule);
      default = {};
      description = ''
        Named islands configuration. Each island is an independent Ogygia
        configuration with its own domain, ZooKeeper settings, irisd instance,
        and CLI configuration.

        Example:
        ```nix
        ogygia.islands."main" = {
          domain = "main.example.com";
          nebula.ipv4 = "172.20.0.1";
          zookeeper.endpoints = [ "zk1:2181" ];
          irisd.enable = true;
        };
        ```
      '';
    };

    # Internal: merged island configs (includes legacy conversion)
    _internalIslands = lib.mkOption {
      internal = true;
      type = lib.types.attrsOf (lib.types.submodule islandModule);
      default = {};
      description = "Merged island configurations including legacy conversion.";
    };
  };

  config = lib.mkMerge [
    # Set _internalIslands from merged config
    (lib.mkIf (allIslands != {}) {
      ogygia._internalIslands = allIslands;
    })

    # Enable defaults for legacy config
    (lib.mkIf hasLegacyConfig {
      ogygia.versions.enable = lib.mkOverride 999 true;
      ogygia.cliConfig.enable = lib.mkOverride 999 true;
      ogygia.versions.build_revision.enable = lib.mkOverride 999 true;
    })

    # Generate config files and other island-specific config
    (lib.mkIf (allIslands != {}) {
      assertions = islandConfigAssertions;

      # Per-island: set cliConfig.package for each enabled island
      ogygia._internalIslands = lib.mapAttrs (name: islandCfg: lib.mkIf islandCfg.enable {
        cliConfig.package = mkIslandConfig name islandCfg;
      }) allIslands;
    })
  ];
}
