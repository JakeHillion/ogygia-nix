{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
in
{
  options.ogygia = {
    enable = lib.mkEnableOption "ogygia";

    domain = lib.mkOption {
      type = lib.types.str;
      description = "Domain name";
      example = "island.example.com";
    };

    gitRemoteUrl = lib.mkOption {
      type = lib.types.str;
      description = "Git remote URL for the NixOS configuration repository.";
      example = "https://git.example.com/user/nixos.git";
    };

    nebula.ipv4 = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "This node's Nebula IPv4 address for internal P2P communication.";
      example = "172.20.0.1";
    };
  };

  imports = [
    ./versions
    ./config
    ./config-generator
    ./etcd
    ./irisd
    ./hostinfod
    ./dashboard
  ];

  config = lib.mkIf cfg.enable {
    ogygia.versions.enable = lib.mkOverride 999 true;
    ogygia.cliConfig.enable = lib.mkOverride 999 true;
    ogygia.hostinfod.enable = lib.mkOverride 999 (cfg.etcd.endpoints != [ ]);
  };
}
