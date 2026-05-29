{ config, lib, pkgs, ... }:

{
  options.ogygia.cliConfig = {
    enable = lib.mkEnableOption "render shared configuration for the Ogygia CLI and tooling";

    package = lib.mkOption {
      internal = true;
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Derivation containing the generated Ogygia CLI configuration.";
    };
  };

  imports = [
    (lib.mkRemovedOptionModule [ "ogygia" "zookeeper" "enable" ]
      "ZooKeeper support has been removed. Please migrate to etcd. Set ogygia.etcd.endpoints to enable etcd integration instead."
    )
    (lib.mkRemovedOptionModule [ "ogygia" "zookeeper" "endpoints" ]
      "ZooKeeper support has been removed. Please migrate to etcd. Set ogygia.etcd.endpoints to enable etcd integration instead."
    )
    (lib.mkRemovedOptionModule [ "ogygia" "zookeeper" "namespace" ]
      "ZooKeeper support has been removed. Please migrate to etcd. Use ogygia.etcd.namespace for the etcd key prefix."
    )
    (lib.mkRemovedOptionModule [ "ogygia" "zookeeper" "timeoutSeconds" ]
      "ZooKeeper support has been removed. Please migrate to etcd. Use ogygia.etcd.timeoutSeconds for the etcd connection timeout."
    )
  ];
}
