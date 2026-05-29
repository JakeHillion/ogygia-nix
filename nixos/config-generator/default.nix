{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  cliCfg = cfg.cliConfig;

  tomlFormat = pkgs.formats.toml { };

  # Build config data from etcd backend
  etcdEnabled = cfg.etcd.endpoints != [ ];
  configData = {
    ogygia = {
      domain = cfg.domain;
    } // lib.optionalAttrs etcdEnabled {
      etcd = {
        endpoints = cfg.etcd.endpoints;
        namespace = cfg.etcd.namespace;
        timeout_seconds = cfg.etcd.timeoutSeconds;
      };
    };
  };

  generatedToml = tomlFormat.generate "config.toml" configData;

  configPackage = pkgs.runCommand "share-ogygia-config" { } ''
    mkdir -p $out/share/ogygia
    cp ${generatedToml} $out/share/ogygia/config.toml
  '';
in
{
  # This module provides the shared config package that the etcd backend contributes to
  config = lib.mkIf (cfg.enable && cliCfg.enable && etcdEnabled) {
    ogygia.cliConfig.package = configPackage;

    environment = {
      systemPackages = [ configPackage ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
