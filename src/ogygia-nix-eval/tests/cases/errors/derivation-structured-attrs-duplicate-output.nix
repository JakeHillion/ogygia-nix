(derivation {
  name = "b";
  system = "x";
  builder = "/bin/sh";
  __structuredAttrs = true;
  outputs = [ "out" "out" ];
}).drvPath
