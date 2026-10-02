# nix-path: nixpkgs=@NIXPKGS@
let
  eval = import <nixpkgs/nixos/lib/eval-config.nix> {
    system = "x86_64-linux";
    modules = [
      ../../../../../nixos/nebula
      ({ lib, ... }: {
        networking.hostName = "boron";
        networking.domain = "cx.example.com";
        system.stateVersion = "25.05";
        ogygia.nebula = {
          enable = true;
          pubKey = "-----BEGIN NEBULA X25519 PUBLIC KEY-----\nAAAA\n-----END NEBULA X25519 PUBLIC KEY-----\n";
          groups = [ "ssh" "etcd-client" ];
          certDir = ../fixtures/nebula;
          topology = {
            subnet = "172.20.0.0/24";
            hosts."boron.cx.example.com".ipv4 = "172.20.0.1";
            hosts."light.cx.example.com" = { ipv4 = "172.20.0.2"; endpoint = "light.example.com:4242"; };
            lighthouses = [ "light.cx.example.com" ];
          };
        };
      })
      ({ lib, ... }: { ogygia.nebula.groups = lib.mkAfter [ "ssh" "zzz" ]; })
    ];
  };
in
{
  inherit (eval.config.ogygia.nebula) enable spec specHash ipv4 validitySecs;
  fqdn = eval.config.networking.fqdnOrHostName;
}
