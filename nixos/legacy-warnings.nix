{ config, lib, ... }:

let
  cfg = config.ogygia;

  # Check if legacy options are being used
  hasLegacyEnable = cfg.enable;
  hasLegacyDomain = cfg.domain != null;
  hasLegacyNebula = cfg.nebula.ipv4 != null;
  hasLegacyZookeeper = cfg.zookeeper.enable;
  hasLegacyIrisd = cfg.irisd.enable;
  hasLegacyCliConfig = cfg.cliConfig.enable && !cfg.cliConfig.package == null;
  hasLegacyVersions = cfg.versions.enable;

  # Build specific deprecation warnings
  specificWarnings = lib.optional hasLegacyEnable
    "ogygia.enable = true is deprecated. Use ogygia.islands.<name>.enable = true instead."
  ++ lib.optional hasLegacyDomain
    "ogygia.domain is deprecated. Use ogygia.islands.<name>.domain instead."
  ++ lib.optional hasLegacyNebula
    "ogygia.nebula.ipv4 is deprecated. Use ogygia.islands.<name>.nebula.ipv4 instead."
  ++ lib.optional hasLegacyZookeeper
    "ogygia.zookeeper is deprecated. Use ogygia.islands.<name>.zookeeper instead."
  ++ lib.optional hasLegacyIrisd
    "ogygia.irisd is deprecated. Use ogygia.islands.<name>.irisd instead."
  ++ lib.optional hasLegacyVersions
    "ogygia.versions is deprecated. Use ogygia.islands.<name>.versions instead.";
in
{
  config = lib.mkIf (specificWarnings != [ ]) {
    warnings = specificWarnings ++ [
      "See the Ogygia migration guide for details on converting to the new islands configuration format."
    ];
  };
}
