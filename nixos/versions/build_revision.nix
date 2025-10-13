{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.versions.build_revision;
in
{
  options.ogygia.versions.build_revision = {
    enable = lib.mkEnableOption "embedding the build revision in the system closure";
  };

  config = lib.mkIf cfg.enable {
    environment = {
      systemPackages = [ (pkgs.writeTextDir "share/ogygia/build-revision" ((config.system.configurationRevision or "dirty") + "\n")) ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
