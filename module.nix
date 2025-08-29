{ config, lib, pkgs, ... }:

with lib;

{
  options.ogygia = {
    enable = mkEnableOption "ogygia";

    domain = mkOption {
      type = types.str;
      description = "Domain name";
      example = "island.example.com";
    };
  };

  config = mkIf config.ogygia.enable {
    # Configuration will be implemented here later
  };
}
