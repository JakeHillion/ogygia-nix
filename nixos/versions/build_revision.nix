{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.versions.build_revision;
  revision = if config.system.configurationRevision != null then config.system.configurationRevision else "unknown";
in
{
  options.ogygia.versions.build_revision = {
    enable = lib.mkEnableOption "embedding the build revision in the system closure";
  };

  config = lib.mkIf cfg.enable {
    environment = {
      systemPackages = [ (pkgs.writeTextDir "share/ogygia/build-revision" (revision + "\n")) ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
