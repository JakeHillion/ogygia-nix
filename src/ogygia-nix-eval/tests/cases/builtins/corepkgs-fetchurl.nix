let
  f = import <nix/fetchurl.nix>;
  show = d: { inherit (d) drvPath outPath; attrs = d.drvAttrs; };
in
[
  <nix/fetchurl.nix>
  (builtins.functionArgs f)
  (show (f { url = "http://example.com/x.tar.gz"; sha256 = "0000000000000000000000000000000000000000000000000000"; }))
  (show (f { url = "http://example.com/y"; hash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="; executable = true; name = "n"; }))
  (show (f { url = "http://e/z"; sha1 = "0000000000000000000000000000000000000000"; unpack = true; }))
  (show (f { url = "http://e/w"; outputHash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="; outputHashAlgo = "sha256"; system = "x86_64-linux"; }))
]
