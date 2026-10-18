# `outputHashAlgo = "blake3"` fails even when the hash names its algorithm.
(derivation {
  name = "d";
  system = "x";
  builder = "/bin/sh";
  outputHash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
  outputHashAlgo = "blake3";
}).outPath
