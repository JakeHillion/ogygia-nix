builtins.tryEval (
  builtins.listToAttrs [
    { name = "a"; }
    { name = throw "n"; }
  ]
)
