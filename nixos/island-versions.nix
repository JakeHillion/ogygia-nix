{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  islands = cfg._internalIslands;

  revision = if config.system.configurationRevision != null 
    then config.system.configurationRevision 
    else "unknown";

  # Generate build revision package for a specific island
  mkIslandBuildRevision = islandName: islandCfg:
    pkgs.runCommand "share-ogygia-build-revision-${islandName}" { } ''
      mkdir -p $out/share/ogygia
      echo "${revision}" > $out/share/ogygia/build-revision-${islandName}
    '';

  # Filter to only enabled islands with versions and build_revision enabled
  enabledIslands = lib.filterAttrs 
    (name: islandCfg: 
      islandCfg.enable && 
      islandCfg.versions.enable && 
      islandCfg.versions.build_revision.enable
    ) 
    islands;

  # Create combined package with all island build revisions
  allRevisionsPackage = pkgs.runCommand "share-ogygia-all-revisions" { } (
    ''
      mkdir -p $out/share/ogygia
    '' +
    lib.concatMapStringsSep "\n"
      (islandName:
        ''cp ${mkIslandBuildRevision islandName enabledIslands.${islandName}}/share/ogygia/* $out/share/ogygia/''
      )
      (lib.attrNames enabledIslands)
  );
in
{
  config = lib.mkIf (enabledIslands != {}) {
    # Make all build revision files available
    environment = {
      systemPackages = [ allRevisionsPackage ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
