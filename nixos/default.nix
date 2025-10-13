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
  };

  imports = [
    ./versions
  ];

  config = lib.mkIf cfg.enable {
    ogygia.versions.enable = lib.mkOverride 999 true;
  };
}
