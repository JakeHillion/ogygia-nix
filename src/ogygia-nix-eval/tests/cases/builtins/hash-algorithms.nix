# Each algorithm through hashString, convertHash with a matching type
# prefix, and the path of a fixed-output derivation, flat and recursive.
map (
  algo:
  let
    hash = builtins.hashString algo "";
    drv =
      outputHashMode:
      (derivation {
        name = "d";
        system = "x";
        builder = "/bin/sh";
        outputHash = hash;
        outputHashAlgo = algo;
        inherit outputHashMode;
      }).outPath;
  in
  [
    hash
    (builtins.convertHash {
      hash = "${algo}:${hash}";
      hashAlgo = algo;
      toHashFormat = "sri";
    })
    (drv "flat")
    (drv "recursive")
  ]
) [ "md5" "sha1" "sha256" "sha512" ]
