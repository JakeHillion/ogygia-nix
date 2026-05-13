{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.nebula;
  topology = cfg.topology;

  # Per nixos/'s pattern, every host in the fleet imports the same `topology`
  # block and self-identifies against it via its FQDN. `fqdnOrHostName` falls
  # back to the bare hostname when `networking.domain` is unset.
  hostFqdn = config.networking.fqdnOrHostName;
  hostRecord = topology.hosts.${hostFqdn} or null;

  isLighthouse = builtins.elem hostFqdn topology.lighthouses;
  isRelay = builtins.elem hostFqdn topology.relays;

  hostIpv4 = if hostRecord != null then hostRecord.ipv4 else null;
  hostSubnet = topology.subnet;

  ready =
    cfg.enable && cfg.pubKey != null && hostIpv4 != null
    && hostSubnet != null && cfg.certDir != null;

  caCertFile = if cfg.certDir != null then cfg.certDir + "/ca.crt" else null;

  sortedGroups = lib.lists.unique (lib.sort (a: b: a < b) cfg.groups);

  spec =
    if !ready then null
    else {
      name = hostFqdn;
      ipv4 = hostIpv4;
      subnet = hostSubnet;
      pubKey = cfg.pubKey;
      groups = sortedGroups;
      caFingerprint = builtins.hashFile "sha256" caCertFile;
      version = 2;
    };

  specHash =
    if spec == null then null
    else builtins.substring 0 32 (builtins.hashString "sha256" (builtins.toJSON spec));

  certPath =
    if specHash == null then null
    else cfg.certDir + "/${specHash}.crt";

  # Auto-derive lighthouse/relay/staticHostMap from the shared topology block.
  # On lighthouses themselves we leave the lists empty (they serve, not query).
  derivedLighthouseIps =
    if isLighthouse then [ ]
    else map (l: topology.hosts.${l}.ipv4) topology.lighthouses;

  derivedRelayIps =
    if isRelay then [ ]
    else map (r: topology.hosts.${r}.ipv4) topology.relays;

  derivedStaticHostMap = lib.listToAttrs (map
    (l: lib.nameValuePair topology.hosts.${l}.ipv4 [ topology.hosts.${l}.endpoint ])
    topology.lighthouses);

  firewallRuleType = lib.types.submodule {
    freeformType = (pkgs.formats.yaml { }).type;
  };

  hostType = lib.types.submodule {
    options = {
      ipv4 = lib.mkOption {
        type = lib.types.str;
        description = "This host's Nebula IPv4 address (no CIDR mask).";
        example = "10.42.0.1";
      };
      endpoint = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          Publicly reachable `host:port` for this node. Required when the host
          appears in `topology.lighthouses` (so other hosts can bootstrap a
          tunnel via the public internet). Ignored for non-lighthouses.
        '';
        example = "lighthouse-1.public.example.com:4242";
      };
    };
  };

in
{
  options.ogygia.nebula = {
    enable = lib.mkEnableOption "ogygia-managed Nebula overlay network";

    pubKey = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        PEM-formatted Nebula public key for this host. Capture once with
        `ogygia nebula keygen <host>` then paste verbatim. Must be set when
        `ogygia.nebula.enable = true`; the build fails otherwise.
      '';
    };

    groups = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Group tags to embed in this host's Nebula certificate. Any module
        in the host's configuration can append to this list — groups are
        deduplicated and sorted before being incorporated into the cert
        spec hash.
      '';
      example = [ "ssh" "etcd-server" ];
    };

    firewall.inbound = lib.mkOption {
      type = lib.types.listOf firewallRuleType;
      default = [ ];
      description = ''
        Inbound Nebula firewall rules. Mergeable across modules so service
        modules can ship their own rules alongside the cert groups they need.
      '';
      example = [{ port = 22; proto = "tcp"; groups = [ "ssh" ]; }];
    };

    firewall.outbound = lib.mkOption {
      type = lib.types.listOf firewallRuleType;
      default = [{ port = "any"; proto = "any"; host = "any"; }];
      description = "Outbound Nebula firewall rules. Default: allow-all.";
    };

    certDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        Directory containing the Nebula CA cert and host certificates.
        Layout (certs are content-addressed by specHash; the spec includes
        the host's pubKey so the hash is globally unique):

          <certDir>/ca.crt
          <certDir>/<specHash>.crt

        Typically set once in a shared module imported by every host.
      '';
      example = lib.literalExpression "./nebula";
    };

    topology = {
      subnet = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          CIDR of the Nebula mesh, e.g. "10.42.0.0/16". Combined with each
          host's `topology.hosts.<fqdn>.ipv4` to form the certificate's
          `-networks` spec at signing time.
        '';
        example = "10.42.0.0/16";
      };

      hosts = lib.mkOption {
        type = lib.types.attrsOf hostType;
        default = { };
        description = ''
          Every host in the mesh, keyed by FQDN — must match the value of
          `config.networking.fqdnOrHostName` on that host. Set fleet-wide
          via a shared module imported by every node.
        '';
      };

      lighthouses = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = ''
          FQDNs of hosts that act as lighthouses. Each must appear in
          `topology.hosts` with `endpoint` set.
        '';
      };

      relays = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "FQDNs of hosts that act as Nebula relays.";
      };
    };

    # Read-only derived values. `ipv4` is exposed for consumers like
    # `ogygia.irisd` that need this host's Nebula address as a bare IP;
    # `spec`/`specHash`/`certPath` are surfaced for the `ogygia nebula` CLI
    # via `nix eval`. All null when the host has no topology record.
    ipv4 = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      readOnly = true;
      default = hostIpv4;
      description = "This host's Nebula IPv4 address (derived from topology.hosts).";
    };

    spec = lib.mkOption {
      type = lib.types.nullOr lib.types.attrs;
      readOnly = true;
      default = spec;
      description = "Canonical cert spec; hashed into specHash.";
    };

    specHash = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      readOnly = true;
      default = specHash;
      description = "First 32 hex chars of sha256(toJSON spec).";
    };

    certPath = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      readOnly = true;
      default = certPath;
      description = "Path to this host's content-addressed certificate.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.pubKey != null;
        message = ''
          ogygia.nebula.pubKey must be set when ogygia.nebula.enable is true.

          Bootstrap: set `ogygia.nebula.enable = lib.mkForce false;` on this
          host, switch once, then run `ogygia nebula keygen <host>` and paste
          the printed public key into `ogygia.nebula.pubKey`.
        '';
      }
      {
        assertion = cfg.certDir != null;
        message = "ogygia.nebula.certDir must be set when ogygia.nebula.enable is true.";
      }
      {
        assertion = topology.subnet != null;
        message = "ogygia.nebula.topology.subnet must be set (e.g. \"10.42.0.0/16\").";
      }
      {
        assertion = hostRecord != null;
        message = ''
          this host (${hostFqdn}) is not enrolled in ogygia.nebula.topology.hosts.
          Add an entry `topology.hosts."${hostFqdn}" = { ipv4 = "..."; };` in
          your shared fleet module.
        '';
      }
    ] ++ map
      (l: {
        assertion = (topology.hosts.${l} or null) != null
          && (topology.hosts.${l}.endpoint or null) != null;
        message = "lighthouse ${l} must appear in ogygia.nebula.topology.hosts with `endpoint` set.";
      })
      topology.lighthouses;

    systemd.tmpfiles.rules = [ "d /etc/nebula 0700 root root - -" ];

    services.nebula.networks.ogygia = {
      ca = caCertFile;
      cert = certPath;
      key = "/etc/nebula/host.key";
      inherit isLighthouse isRelay;
      lighthouses = derivedLighthouseIps;
      relays = derivedRelayIps;
      staticHostMap = derivedStaticHostMap;
      tun.device = "neb.ogygia";
      listen = {
        host = "[::]";
        # Lighthouses bind a known port so clients can reach them via
        # staticHostMap; other hosts use an ephemeral port (Nebula's default
        # when not lighthousing/relaying).
        port = if isLighthouse || isRelay then 4242 else 0;
      };
      firewall = {
        inbound = cfg.firewall.inbound;
        outbound = cfg.firewall.outbound;
      };
    };

    # The mesh handles its own host-firewall via the cert-group rules above;
    # let local services bind freely on the tun.
    networking.firewall.trustedInterfaces = [ "neb.ogygia" ];
  };
}
