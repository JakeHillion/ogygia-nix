# An output may be called drv.
(derivation {
  name = "b";
  system = "x";
  builder = "/bin/sh";
  outputs = [ "out" "drv" ];
}).drv.outPath
