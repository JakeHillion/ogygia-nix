# Structured attrs take outputs as a list and the output hash settings as
# strings, like plain attrs.
let
  s = {
    system = "x86_64-linux";
    builder = "/bin/sh";
    __structuredAttrs = true;
  };
  multi = derivation (s // { name = "multi"; outputs = [ "out" "dev" ]; });
  fixed = derivation (s // {
    name = "fixed";
    outputHashMode = "recursive";
    outputHashAlgo = "sha256";
    outputHash = "0000000000000000000000000000000000000000000000000000";
  });
in
map (d: { inherit (d) drvPath outPath; outs = map (o: d.${o}.outPath) (d.outputs or [ "out" ]); }) [ multi multi.dev fixed ]
