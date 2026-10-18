# `outputHashAlgo = "blake3"` fails when the attribute is processed, before
# later attributes are forced, even without `outputHash`.
builtins.tryEval (derivation {
  name = "d";
  system = "x";
  builder = "/bin/sh";
  outputHashAlgo = "blake3";
  z = throw "z";
}).outPath
