{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.nebula;

  relayHostnames = cfg.relays;

  serviceUser = config.systemd.services."nebula@ogygia".serviceConfig.User;
  serviceGroup = config.systemd.services."nebula@ogygia".serviceConfig.Group;
in
{
  options.ogygia.nebula = {
    enable = lib.mkEnableOption "nebula mesh network";

    lighthouses = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = config.ogygia.pleiades;
      description = "Mapping of ogygia domain names to public DNS names for lighthouses";
    };

    lighthousePort = lib.mkOption {
      type = lib.types.port;
      default = 4242;
      description = "Port for lighthouse communication";
    };

    relays = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "List of relay hostnames";
    };

    certPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/ogygia/nebula/host.crt";
      description = "Path to host certificate";
    };

    keyPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/ogygia/nebula/host.key";
      description = "Path to host private key";
    };

    caPath = lib.mkOption {
      type = lib.types.path;
      description = "Path to certificate authority";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.${serviceUser}.uid = config.ids.uids.${serviceUser};
    users.groups.${serviceGroup}.gid = config.ids.gids.${serviceGroup};

    systemd.tmpfiles.rules = [
      "d ${builtins.dirOf cfg.certPath} 0775 ${serviceUser} ${serviceGroup} - -"
      "d ${builtins.dirOf cfg.keyPath} 0775 ${serviceUser} ${serviceGroup} - -"
    ];

    systemd.services.generate-nebula-certs = {
      description = "Generate empty Nebula certificates if they don't exist";

      before = [ "nebula@ogygia.service" ];
      wantedBy = [ "multi-user.target" ];

      script = ''
        if [ ! -e ${cfg.certPath} ] && [ ! -e ${cfg.keyPath} ]; then
          ${pkgs.nebula}/bin/nebula-cert keygen -out-key ${cfg.keyPath} -out-pub ${cfg.certPath}
        fi

        chown ${serviceUser}:${serviceGroup} ${cfg.keyPath} ${cfg.certPath}
        chmod 0400 ${cfg.keyPath}
        chmod 0444 ${cfg.certPath}
      '';
    };

    # Turn off the normal firewall and use the Nebula capability based firewall instead.
    networking.firewall.trustedInterfaces = [ "ogygia" ];

    services.nebula.networks =
      let
        isLighthouse = lighthouses ? ${config.networking.fqdn};
        isRelay = lib.lists.any (x: config.networking.fqdn == x) relayHostnames;
      in
      {
        "ogygia" = {
          enable = true;
          tun.device = "ogygia";

          ca = cfg.caPath;
          cert = cfg.certPath;
          key = cfg.keyPath;

          inherit isLighthouse isRelay;

          lighthouses = lib.lists.optionals (!isLighthouse) (builtins.attrNames lighthouses);

          relays = lib.lists.optionals (!isRelay) relayHostnames;

          listen = lib.mkMerge [
            { host = "[::]"; }

            (lib.mkIf isLighthouse {
              port = cfg.lighthousePort;
            })
          ];

          staticHostMap = lib.mapAttrs (ogygiaName: publicName: [ "${publicName}:${toString cfg.lighthousePort}" ]) lighthouses;


          firewall = {
            inbound = [{ host = "any"; port = "any"; proto = "any"; }];
            outbound = [{ host = "any"; port = "any"; proto = "any"; }];
          };
        };
      };
  };
}
