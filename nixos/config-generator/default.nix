{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia;
  cliCfg = cfg.cliConfig;

  tomlFormat = pkgs.formats.toml { };

  # Build config data from all enabled backends
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
    } // lib.optionalAttrs cfg.zookeeper.enable {
      zookeeper = {
        endpoints = cfg.zookeeper.endpoints;
        namespace = cfg.zookeeper.namespace;
        timeout_seconds = cfg.zookeeper.timeoutSeconds;
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
  # This module provides the shared config package that all backends contribute to
  config = lib.mkIf (cfg.enable && cliCfg.enable && (etcdEnabled || cfg.zookeeper.enable)) {
    ogygia.cliConfig.package = configPackage;

    environment = {
      systemPackages = [ configPackage ];
      pathsToLink = [ "/share/ogygia" ];
    };
  };
}
