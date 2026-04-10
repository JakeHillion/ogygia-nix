{ config, lib, pkgs, ... }:

let
  cfg = config.ogygia.nebula;

  # Compute the identity hash for content-addressed certificate lookup
  # This must match the Rust implementation in src/nebula/cert.rs
  computeIdentityHash = pubKeyFingerprint: ip: groups: fqdn:
    let
      sortedGroups = lib.sort lib.lessThan groups;
      hashInput = lib.concatStrings [
        pubKeyFingerprint
        ip
        fqdn
        (lib.concatStrings sortedGroups)
      ];
    in
      lib.substring 0 32 (builtins.hashString "sha256" hashInput);

  # Get public key fingerprint (would be extracted from host key during activation)
  # For now, this is computed during activation script
  hostKeyPath = cfg.hostKeyPath;
  hostPubKeyPath = cfg.hostKeyPath + ".pub";

  # Certificate path is computed at eval time but the actual file
  # is looked up at activation time based on host's public key
  # This creates the content-addressed lookup mechanism
  certDir = cfg.rekeyedDir or "./nebula/rekeyed";

  # Service name for nebula network
  networkName = "ogygia";
in
{
  options.ogygia.nebula = {
    enable = lib.mkEnableOption "Ogygia-managed Nebula certificate and overlay network";

    ip = lib.mkOption {
      type = lib.types.str;
      description = ''
        Nebula IP address with CIDR for this host.
        This is part of the certificate identity and affects the content-addressed hash.
      '';
      example = "172.20.0.1/24";
    };

    groups = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = ''
        Nebula groups for firewall rules.
        These are part of the certificate identity and affect the content-addressed hash.
        Common groups include "lighthouse", "relay", "server", "client".
      '';
      example = [ "lighthouse" "server" ];
    };

    caCert = lib.mkOption {
      type = lib.types.path;
      default = ./nebula/ca.crt;
      description = ''
        Path to the Nebula CA certificate (public).
        This file should be committed to your repository.
      '';
    };

    rekeyedDir = lib.mkOption {
      type = lib.types.path;
      default = ./nebula/rekeyed;
      description = ''
        Path to the directory containing content-addressed rekeyed certificates.
        Certificates in this directory follow the pattern:
        <hash>.<fqdn>.crt
        where <hash> is computed from (pubKey + ip + groups + fqdn).
      '';
    };

    hostKeyPath = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/ogygia/nebula/host.key";
      description = ''
        Path where the host's Nebula private key is stored.
        The corresponding public key will be at this path + ".pub".
        This key is generated automatically if it doesn't exist.
      '';
    };

    exposePublicKey = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Make the host's public key available for retrieval during certificate signing.
        The public key is placed at /var/lib/ogygia/nebula/host.pub.
      '';
    };

    # Computed option - the actual certificate file path (content-addressed)
    certFile = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      description = ''
        Content-addressed path to the certificate file for this host.
        This is computed at activation time based on the host's public key fingerprint
        and the configured IP, groups, and FQDN.
      '';
    };

    # Network configuration passed through to services.nebula
    isLighthouse = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether this host is a Nebula lighthouse.";
    };

    isRelay = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether this host is a Nebula relay.";
    };

    lighthouses = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "List of lighthouse IPs (only used on non-lighthouse hosts).";
    };

    staticHostMap = lib.mkOption {
      type = lib.types.attrsOf (lib.types.listOf lib.types.str);
      default = {};
      description = ''
        Static mapping of lighthouse IPs to their external addresses.
        Example: { "172.20.0.1" = [ "lighthouse.example.com:4242" ]; }
      '';
    };

    firewall = {
      inbound = lib.mkOption {
        type = lib.types.listOf lib.types.attrs;
        default = [];
        description = "Inbound firewall rules (nebula network only).";
      };

      outbound = lib.mkOption {
        type = lib.types.listOf lib.types.attrs;
        default = [{ host = "any"; port = "any"; proto = "any"; }];
        description = "Outbound firewall rules.";
      };
    };

    listen = {
      host = lib.mkOption {
        type = lib.types.str;
        default = "[::]";
        description = "Host to listen on.";
      };

      port = lib.mkOption {
        type = lib.types.nullOr lib.types.int;
        default = null;
        description = ''
          Port to listen on. If null, uses ephemeral port (non-lighthouses).
          Set to 4242 for lighthouses.
        '';
      };
    };

    tun = {
      device = lib.mkOption {
        type = lib.types.str;
        default = "neb.ogygia";
        description = "TUN device name for Nebula interface.";
      };

      disabled = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Disable TUN device (useful for mobile clients).";
      };
    };

    extraSettings = lib.mkOption {
      type = lib.types.attrs;
      default = {};
      description = "Extra settings passed directly to Nebula configuration.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.ip != "";
        message = "ogygia.nebula.ip must be set";
      }
    ];

    # Compute certFile at eval time
    # Note: The actual file existence is checked at activation time
    ogygia.nebula.certFile = 
      let
        # We can't know the public key fingerprint at eval time,
        # so we use a placeholder that will be resolved at activation
        # The activation script will compute the actual hash
        identityPrefix = builtins.hashString "sha256" (cfg.ip + config.networking.fqdn + lib.concatStrings cfg.groups);
      in
        cfg.rekeyedDir + "/" + identityPrefix + "." + config.networking.fqdn + ".crt";

    # Create service user for Nebula
    users.users."nebula-${networkName}" = {
      uid = config.ids.uids."nebula-${networkName}" or (lib.mkForce 900);
      group = "nebula-${networkName}";
      description = "Nebula overlay network daemon for ${networkName}";
    };

    users.groups."nebula-${networkName}" = {
      gid = config.ids.gids."nebula-${networkName}" or (lib.mkForce 900);
    };

    # Ensure directories exist
    systemd.tmpfiles.rules = [
      "d /var/lib/ogygia/nebula 0755 nebula-${networkName} nebula-${networkName} -"
    ];

    # Generate keypair if missing
    systemd.services."nebula-${networkName}-keygen" = {
      description = "Generate Nebula keypair for ${networkName}";
      before = [ "nebula@${networkName}.service" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        User = "nebula-${networkName}";
        Group = "nebula-${networkName}";
      };
      script = ''
        set -e
        KEY_FILE="${cfg.hostKeyPath}"
        PUB_FILE="${hostPubKeyPath}"
        
        if [ ! -e "$KEY_FILE" ] && [ ! -e "$PUB_FILE" ]; then
          echo "Generating new Nebula keypair..."
          ${pkgs.nebula}/bin/nebula-cert keygen -out-key "$KEY_FILE" -out-pub "$PUB_FILE"
        fi
        
        # Ensure proper permissions
        if [ -e "$KEY_FILE" ]; then
          chmod 0400 "$KEY_FILE"
          chown nebula-${networkName}:nebula-${networkName} "$KEY_FILE"
        fi
        
        if [ -e "$PUB_FILE" ]; then
          chmod 0444 "$PUB_FILE"
          chown nebula-${networkName}:nebula-${networkName} "$PUB_FILE"
        fi
        
        # Expose public key for certificate signing if enabled
        ${lib.optionalString cfg.exposePublicKey ''
          mkdir -p /var/lib/ogygia/nebula
          cp "$PUB_FILE" /var/lib/ogygia/nebula/host.pub
          chmod 0444 /var/lib/ogygia/nebula/host.pub
        ''}
      '';
    };

    # Check certificate exists before starting Nebula
    systemd.services."nebula-${networkName}-check-cert" = {
      description = "Check Nebula certificate exists for ${networkName}";
      before = [ "nebula@${networkName}.service" ];
      after = [ "nebula-${networkName}-keygen.service" ];
      requiredBy = [ "nebula@${networkName}.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        # Compute actual content-addressed path based on public key
        PUB_FILE="${hostPubKeyPath}"
        
        if [ ! -e "$PUB_FILE" ]; then
          echo "ERROR: Host public key not found at $PUB_FILE"
          echo "The keygen service should have created this. Check the logs."
          exit 1
        fi
        
        # Read public key content
        PUB_KEY=$(cat "$PUB_FILE")
        
        # Compute fingerprint (first 8 bytes of SHA256 of base64 decoded key)
        FINGERPRINT=$(echo "$PUB_KEY" | base64 -d 2>/dev/null | sha256sum | head -c 16)
        
        # Compute identity hash matching the Rust implementation
        # hash = sha256(fingerprint + ip + fqdn + sorted_groups)
        IP="${cfg.ip}"
        FQDN="${config.networking.fqdn}"
        GROUPS="${lib.concatStrings cfg.groups}"
        IDENTITY_HASH=$(echo -n "''${FINGERPHONE}${IP}${FQDN}${GROUPS}" | sha256sum | head -c 32)
        
        CERT_FILE="${cfg.rekeyedDir}/''${IDENTITY_HASH}.${FQDN}.crt"
        
        if [ ! -e "$CERT_FILE" ]; then
          echo "ERROR: Nebula certificate not found at expected location:"
          echo "  $CERT_FILE"
          echo ""
          echo "This is a content-addressed certificate. The path is computed from:"
          echo "  - Host public key fingerprint"
          echo "  - IP address: ${cfg.ip}"
          echo "  - FQDN: ${config.networking.fqdn}"
          echo "  - Groups: [${lib.concatStringsSep ", " cfg.groups}]"
          echo ""
          echo "To generate the certificate, run on your development machine:"
          echo "  ogygia nebula discover  # to see the public key"
          echo "  ogygia nebula rekey ${config.networking.fqdn} --ca-key /path/to/ca.key"
          echo ""
          echo "Then commit the generated certificate and redeploy."
          exit 1
        fi
        
        echo "Certificate found: $CERT_FILE"
        
        # Create a stable symlink that Nebula can use
        mkdir -p /var/lib/ogygia/nebula
        ln -sf "$CERT_FILE" /var/lib/ogygia/nebula/host.crt
      '';
    };

    # Configure Nebula service
    services.nebula.networks.${networkName} = {
      enable = true;
      
      ca = cfg.caCert;
      cert = "/var/lib/ogygia/nebula/host.crt";
      key = cfg.hostKeyPath;
      
      listen = cfg.listen;
      
      isLighthouse = cfg.isLighthouse;
      isRelay = cfg.isRelay;
      lighthouses = cfg.lighthouses;
      
      staticHostMap = cfg.staticHostMap;
      
      tun = cfg.tun;
      
      firewall = cfg.firewall;
      
      settings = cfg.extraSettings;
    };

    # Add nebula-cert to system packages for debugging
    environment.systemPackages = lib.optional cfg.exposePublicKey pkgs.nebula;
  };
}
