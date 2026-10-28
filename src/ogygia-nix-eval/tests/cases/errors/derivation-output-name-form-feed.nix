# Output names are split on space, tab, newline and carriage return only, so
# a form feed stays in the name and its store path name is invalid.
builtins.tryEval
  (derivation {
    name = "b";
    system = "x";
    builder = "/bin/sh";
    outputs = [ "ab" ];
  }).drvPath
