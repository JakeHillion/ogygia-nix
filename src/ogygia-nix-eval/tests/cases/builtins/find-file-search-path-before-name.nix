# Every search path entry is forced and checked before the name is.
builtins.tryEval (
  builtins.findFile [
    { path = "/"; }
    { path = abort "x"; }
  ] (throw "name")
)
