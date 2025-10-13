{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.versions;
in
{
  options.ogygia.versions = {
    enable = lib.mkEnableOption "tooling for tracking versions within the island";
  };

  imports = [
    ./build_revision.nix
  ];

  config = lib.mkIf cfg.enable {
    ogygia.versions.build_revision.enable = lib.mkOverride 999 true;
  };
}
