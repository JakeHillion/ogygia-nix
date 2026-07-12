{ pkgs, lib, ogygiaModule }:

let
  fakeCaCert = pkgs.writeText "ca.crt" "ogygia-test-ca-cert\n";
  caFingerprint = builtins.hashFile "sha256" fakeCaCert;

  expectedSpec = {
    name = "testhost";
    ipv4 = "10.42.0.5";
    subnet = "10.42.0.0/16";
    pubKey = "fake-pub-key-pem";
    groups = [ "etcd-server" "ssh" ];
    inherit caFingerprint;
    version = 2;
  };
  expectedSpecHash = builtins.substring 0 32 (builtins.hashString "sha256" (builtins.toJSON expectedSpec));

  certDir = pkgs.runCommand "ogygia-nebula-test-fixture" { } ''
    mkdir -p $out
    cp ${fakeCaCert} $out/ca.crt
    echo "fake-cert-${expectedSpecHash}" > $out/${expectedSpecHash}.crt
  '';

  sharedTopology = {
    subnet = "10.42.0.0/16";
    hosts = {
      "lighthouse-1" = { ipv4 = "10.42.0.1"; endpoint = "lh1.public.example.com:4242"; };
      "testhost" = { ipv4 = "10.42.0.5"; };
    };
    lighthouses = [ "lighthouse-1" ];
    relays = [ ];
  };

  baseModule = { lib, ... }: {
    nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
    networking.hostName = "testhost";
    system.stateVersion = "25.05";
    boot.loader.grub.enable = false;
    fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
    ogygia.nebula = {
      enable = true;
      pubKey = "fake-pub-key-pem";
      groups = [ "ssh" "etcd-server" ];
      inherit certDir;
      topology = sharedTopology;
    };
  };

  validSystem = lib.nixosSystem {
    modules = [ ogygiaModule baseModule ];
  };

  firewallA = { ... }: {
    ogygia.nebula.firewall.inbound = [
      { port = 22; proto = "tcp"; groups = [ "ssh" ]; }
    ];
  };
  firewallB = { ... }: {
    ogygia.nebula.firewall.inbound = [
      { port = 2379; proto = "tcp"; groups = [ "etcd-server" ]; }
    ];
    ogygia.nebula.groups = [ "client" ];
  };

  mergeSystem = lib.nixosSystem {
    modules = [ ogygiaModule baseModule firewallA firewallB ];
  };

  failedAssertions = sys: builtins.filter (a: !a.assertion) sys.config.assertions;

  noPubKeySystem = lib.nixosSystem {
    modules = [
      ogygiaModule
      ({ ... }: {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        networking.hostName = "testhost";
        system.stateVersion = "25.05";
        boot.loader.grub.enable = false;
        fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
        ogygia.nebula = {
          enable = true;
          inherit certDir;
          topology = sharedTopology;
        };
      })
    ];
  };

  # Build the same fleet from a lighthouse's perspective to check
  # auto-derivation of staticHostMap, isLighthouse, listen.port.
  lighthouseSystem = lib.nixosSystem {
    modules = [
      ogygiaModule
      ({ ... }: {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        networking.hostName = "lighthouse-1";
        system.stateVersion = "25.05";
        boot.loader.grub.enable = false;
        fileSystems."/" = { device = "none"; fsType = "tmpfs"; };
        ogygia.nebula = {
          enable = true;
          pubKey = "fake-lh-pubkey";
          inherit certDir;
          topology = sharedTopology;
        };
      })
    ];
  };
in
pkgs.runCommand "ogygia-nebula-module"
{
  nativeBuildInputs = [ pkgs.jq ];
  inherit expectedSpecHash;
  actualSpecHash = validSystem.config.ogygia.nebula.specHash;
  validCertPath = toString validSystem.config.ogygia.nebula.certPath;
  serviceCert = toString validSystem.config.services.nebula.networks.ogygia.cert;
  serviceCa = toString validSystem.config.services.nebula.networks.ogygia.ca;
  serviceKey = validSystem.config.services.nebula.networks.ogygia.key;
  tunDevice = validSystem.config.services.nebula.networks.ogygia.tun.device;
  trustedIfaces = builtins.toJSON validSystem.config.networking.firewall.trustedInterfaces;
  clientLighthouses = builtins.toJSON validSystem.config.services.nebula.networks.ogygia.lighthouses;
  clientStaticHostMap = builtins.toJSON validSystem.config.services.nebula.networks.ogygia.staticHostMap;
  clientIsLighthouse = lib.boolToString validSystem.config.services.nebula.networks.ogygia.isLighthouse;
  clientListenPort = toString validSystem.config.services.nebula.networks.ogygia.listen.port;
  lighthouseIsLighthouse = lib.boolToString lighthouseSystem.config.services.nebula.networks.ogygia.isLighthouse;
  lighthouseLighthouses = builtins.toJSON lighthouseSystem.config.services.nebula.networks.ogygia.lighthouses;
  lighthouseListenPort = toString lighthouseSystem.config.services.nebula.networks.ogygia.listen.port;
  nebulaOnlineUnitExists = lib.boolToString (validSystem.config.systemd.services ? "nebula-online@ogygia");
  mergedInbound = builtins.toJSON mergeSystem.config.services.nebula.networks.ogygia.firewall.inbound;
  mergedGroups = builtins.toJSON mergeSystem.config.ogygia.nebula.groups;
  noPubFailedCount = toString (builtins.length (failedAssertions noPubKeySystem));
  noPubFailedFirst =
    let fas = failedAssertions noPubKeySystem;
    in if fas == [ ] then "" else (builtins.head fas).message;
} ''
  set -eu

  if [ "$actualSpecHash" != "$expectedSpecHash" ]; then
    echo "specHash mismatch: got '$actualSpecHash', expected '$expectedSpecHash'" >&2
    exit 1
  fi

  if [ ! -f "$validCertPath" ]; then
    echo "certPath does not exist on disk: $validCertPath" >&2
    exit 1
  fi
  if [ "$serviceCert" != "$validCertPath" ]; then
    echo "services.nebula.networks.ogygia.cert ($serviceCert) != certPath ($validCertPath)" >&2
    exit 1
  fi
  if [ ! -f "$serviceCa" ]; then
    echo "services.nebula.networks.ogygia.ca is missing: $serviceCa" >&2
    exit 1
  fi
  if [ "$serviceKey" != "/etc/nebula/host.key" ]; then
    echo "services.nebula.networks.ogygia.key has unexpected value: $serviceKey" >&2
    exit 1
  fi
  if [ "$tunDevice" != "neb.ogygia" ]; then
    echo "tun.device should be neb.ogygia, got: $tunDevice" >&2
    exit 1
  fi
  echo "$trustedIfaces" | jq -e 'index("neb.ogygia") != null' >/dev/null

  # Client-side: non-lighthouse derives the lighthouse Nebula IP and a static
  # host map pointing at the lighthouse's public endpoint.
  if [ "$clientIsLighthouse" != "false" ]; then
    echo "client should not be a lighthouse, got: $clientIsLighthouse" >&2
    exit 1
  fi
  if [ "$clientListenPort" != "0" ]; then
    echo "client should listen on an ephemeral port, got: $clientListenPort" >&2
    exit 1
  fi
  echo "$clientLighthouses" | jq -e '. == ["10.42.0.1"]' >/dev/null
  echo "$clientStaticHostMap" | jq -e '."10.42.0.1" == ["lh1.public.example.com:4242"]' >/dev/null

  # Lighthouse-side: identifies itself and does not list itself in lighthouses.
  if [ "$lighthouseIsLighthouse" != "true" ]; then
    echo "lighthouse-1 should be a lighthouse, got: $lighthouseIsLighthouse" >&2
    exit 1
  fi
  if [ "$lighthouseListenPort" != "4242" ]; then
    echo "lighthouse should listen on 4242, got: $lighthouseListenPort" >&2
    exit 1
  fi
  echo "$lighthouseLighthouses" | jq -e '. == []' >/dev/null

  if [ "$nebulaOnlineUnitExists" != "true" ]; then
    echo "nebula-online@ogygia.service is not defined" >&2
    exit 1
  fi

  echo "$mergedInbound" | jq -e 'length == 2' >/dev/null
  echo "$mergedInbound" | jq -e 'map(select(.port == 22 and (.groups | index("ssh")))) | length == 1' >/dev/null
  echo "$mergedInbound" | jq -e 'map(select(.port == 2379 and (.groups | index("etcd-server")))) | length == 1' >/dev/null

  echo "$mergedGroups" | jq -e 'sort == ["client", "etcd-server", "ssh"]' >/dev/null

  if [ "$noPubFailedCount" -lt "1" ]; then
    echo "expected a failed assertion for missing pubKey" >&2
    exit 1
  fi
  case "$noPubFailedFirst" in
    *pubKey*) ;;
    *) echo "expected pubKey-related assertion message, got: $noPubFailedFirst" >&2; exit 1 ;;
  esac

  mkdir -p $out
  echo "spec_hash=$actualSpecHash" > $out/result
  echo "cert_path=$validCertPath" >> $out/result
''
