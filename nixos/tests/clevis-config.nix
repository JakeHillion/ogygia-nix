{ pkgs, lib, ogygiaModule }:

let
  system = lib.nixosSystem {
    modules = [
      { nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system; }
      ogygiaModule
      {
        ogygia.enable = true;
        ogygia.domain = "neb.test";
        ogygia.clevis = {
          enable = true;
          secretFile = "/data/disk_encryption.jwe";
          spec = {
            t = 1;
            pins.tang = [
              { url = "http://tang1.neb.test:7654"; thp = "H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE"; }
              { url = "http://tang2.neb.test:7654"; thp = "Bai-SYCM1Jg3VJLRCpVyNosWrgSDSlv5HvFmZTTsZms"; }
            ];
          };
        };
      }
    ];
  };

  service = system.config.systemd.services.ogygia-clevis.serviceConfig;

  configFile = system.config.environment.etc."ogygia/clevis.json".source;
in
pkgs.runCommand "ogygia-clevis-config"
{
  nativeBuildInputs = [ pkgs.jq ];
} ''
  case "${service.ExecStart}" in
    *" --config /etc/ogygia/clevis.json") ;;
    *) echo "ExecStart does not use /etc/ogygia/clevis.json: ${service.ExecStart}" >&2; exit 1 ;;
  esac

  # The unit must be able to write the replacement blob next to the
  # original, and nowhere else.
  case "${toString service.ReadWritePaths}" in
    "/data") ;;
    *) echo "ReadWritePaths is not the blob's directory: ${toString service.ReadWritePaths}" >&2; exit 1 ;;
  esac

  config="${configFile}"
  [ "$(jq -r '.secret_file' "$config")" = /data/disk_encryption.jwe ]
  [ "$(jq '.spec.t' "$config")" = 1 ]
  [ "$(jq -r '.spec.pins.tang[0].url' "$config")" = http://tang1.neb.test:7654 ]
  [ "$(jq -r '.spec.pins.tang[1].url' "$config")" = http://tang2.neb.test:7654 ]
  [ "$(jq -r '.spec.pins.tang[0].thp' "$config")" = H9qQk8sByKi5aXUGYVDVXMnH_QV9wSOjMiVnNxqKAyE ]

  mkdir -p $out
  cp "$config" "$out/clevis.json"
''
