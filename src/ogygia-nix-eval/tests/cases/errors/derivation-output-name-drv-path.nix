# An output cannot be called drvPath; tryEval cannot catch the error.
builtins.tryEval
  (derivation {
    name = "b";
    system = "x";
    builder = "/bin/sh";
    outputs = [ "out" "drvPath" ];
  }).outPath
