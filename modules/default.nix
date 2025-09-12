{ config, lib, pkgs, ... }:

{
  imports = [
    ./nebula
  ];

  options.ogygia = {
    enable = mkEnableOption "ogygia";

    domain = mkOption {
      type = types.str;
      description = "Domain name";
      example = "island.example.com";
    };

    pleiades = mkOption {
      type = types.attrsOf types.str;
      description = "Hosts with a public IP we can use for Internet accessible services.";
      example = {
        "lighthouse1.ogygia.example.com" = "lighthouse1.example.com";
        "lighthouse2.ogygia.example.com" = "lighthouse2.example.com";
      };
    };
  };

  config = mkIf config.ogygia.enable {
    ogygia.nebula.enable = true;
  };
}
