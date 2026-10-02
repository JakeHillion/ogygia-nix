let
  a = derivation { name = "a"; system = "x86_64-linux"; builder = "/bin/sh"; args = [ "-c" "echo" ]; };
  b = derivation { name = "b"; system = "x86_64-linux"; builder = "/bin/sh"; dep = a; outputs = [ "out" "dev" ]; list = [ 1 "x" null true ]; flag = false; };
  fixed = derivation { name = "fixed"; system = "x86_64-linux"; builder = "/bin/sh"; outputHash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="; outputHashMode = "recursive"; };
  flat = derivation { name = "flat"; system = "x86_64-linux"; builder = "/bin/sh"; outputHashAlgo = "sha256"; outputHash = "0000000000000000000000000000000000000000000000000000"; };
  c = derivation { name = "c"; system = "x86_64-linux"; builder = "${fixed}/bin/x"; src = ./fixtures/x; };
  d = derivation { name = "d"; system = "x86_64-linux"; builder = "/bin/sh"; inherit c; __structuredAttrs = true; nested = { a = [ 1 2 ]; }; };
  e = derivation { name = "e"; system = "x86_64-linux"; builder = "/bin/sh"; x = null; __ignoreNulls = true; };
in
map (d: { inherit (d) drvPath outPath name type outputName; outs = map (o: d.${o}.outPath) (d.outputs or [ "out" ]); }) [ a b b.dev fixed flat c d e ]
