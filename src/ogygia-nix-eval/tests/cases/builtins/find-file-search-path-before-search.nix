# Every search path entry is forced and checked before any is searched.
builtins.findFile [
  { path = "/"; }
  { prefix = ""; }
] "nix"
