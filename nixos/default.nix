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
    ./irisd
    ./nebula
  ];

  config = lib.mkIf cfg.enable {
    ogygia.versions.enable = lib.mkOverride 999 true;
    ogygia.cliConfig.enable = lib.mkOverride 999 true;
  };
}
