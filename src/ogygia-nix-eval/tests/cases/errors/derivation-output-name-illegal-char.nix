# An output's store path name must be valid; tryEval cannot catch the error.
builtins.tryEval
  (derivation {
    name = "b";
    system = "x";
    builder = "/bin/sh";
    outputs = [ "out" "a:b" ];
  }).drvPath
