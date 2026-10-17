{ config, lib, pkgs, ... }:

# NixOS wiring for ogygia-managed Nebula: runs the network through
# services.nebula from the spec and topology in ./common.nix.

let
  cfg = config.ogygia.nebula;
  derived = cfg.internal;
in
{
  imports = [ ./common.nix ];

  config = lib.mkMerge [
    {
      # Per nixos/'s pattern, every host in the fleet imports the same
      # `topology` block and self-identifies against it via its FQDN.
      # `fqdnOrHostName` falls back to the bare hostname when
      # `networking.domain` is unset.
      ogygia.nebula.name = lib.mkDefault config.networking.fqdnOrHostName;
    }

    (lib.mkIf cfg.enable {
      assertions = derived.assertions;

      systemd.tmpfiles.rules = [ "d /etc/nebula 0755 root root - -" ];

      services.nebula.networks.ogygia = {
        ca = derived.caCertFile;
        cert = cfg.certPath;
        key = "/etc/nebula/host.key";
        inherit (derived) isLighthouse isRelay;
        lighthouses = derived.lighthouseIps;
        relays = derived.relayIps;
        staticHostMap = derived.staticHostMap;
        tun.device = "neb.ogygia";
        listen = {
          host = "[::]";
          port = derived.listenPort;
        };
        firewall = {
          inbound = cfg.firewall.inbound;
          outbound = cfg.firewall.outbound;
        };
      };

      # The mesh handles its own host-firewall via the cert-group rules above;
      # let local services bind freely on the tun.
      networking.firewall.trustedInterfaces = [ "neb.ogygia" ];

      # Convenience unit for downstream services that need the mesh up before
      # they start — `Requires = nebula-online@ogygia.service`.
      systemd.services."nebula-online@ogygia" = {
        description = "Wait for Nebula interface neb.ogygia to come online";
        after = [ "nebula@ogygia.service" ];
        requires = [ "nebula@ogygia.service" ];
        wantedBy = [ "multi-user.target" ];
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          TimeoutStartSec = "60";
        };
        script = ''
          until ${pkgs.iproute2}/bin/ip addr show neb.ogygia 2>/dev/null \
            | ${pkgs.gnugrep}/bin/grep -q "inet "; do
            sleep 1
          done
        '';
      };
    })
  ];
}
