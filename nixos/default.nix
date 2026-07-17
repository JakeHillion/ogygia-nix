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

  };

  imports = [
    ./versions
    ./config
    ./config-generator
    ./etcd
    ./irisd
    ./nebula
    ./hostinfod
    ./dashboard
    ./updated
  ];

  config = lib.mkIf cfg.enable {
    ogygia.versions.enable = lib.mkOverride 999 true;
    ogygia.cliConfig.enable = lib.mkOverride 999 true;
    ogygia.hostinfod.enable = lib.mkOverride 999 (cfg.etcd.endpoints != [ ]);
  };
}
