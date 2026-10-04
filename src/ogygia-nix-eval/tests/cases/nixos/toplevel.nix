# nix-path: nixpkgs=@NIXPKGS@
# Every derivation of a minimal system, down to the bootstrap tools, must
# hash identically for the top-level paths to agree.
let
  eval = import <nixpkgs/nixos/lib/eval-config.nix> {
    system = "x86_64-linux";
    modules = [
      {
        boot.loader.grub.enable = false;
        fileSystems."/" = { device = "/dev/sda"; fsType = "ext4"; };
        system.stateVersion = "25.05";
      }
    ];
  };
in
{
  inherit (eval.config.system.build.toplevel) drvPath outPath;
  etc = eval.config.system.build.etc.drvPath;
}
