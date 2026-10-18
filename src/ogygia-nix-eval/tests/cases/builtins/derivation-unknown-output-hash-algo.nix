# An `outputHashAlgo` that names no hash algorithm is ignored, so a hash
# that names its own algorithm still gives a path.
let
  drv =
    outputHashAlgo: outputHash: outputHashMode:
    (derivation {
      name = "d";
      system = "x";
      builder = "/bin/sh";
      inherit outputHashAlgo outputHash outputHashMode;
    }).outPath;
in
map (a: map (h: map (drv a h) [ "flat" "recursive" ]) [
  "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
  "sha256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
  "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  "sha1:143xibwh31h9bvxzalr0sjvbbvpa6ffs"
]) [ "foo" "SHA256" "r:sha256" "" ]
