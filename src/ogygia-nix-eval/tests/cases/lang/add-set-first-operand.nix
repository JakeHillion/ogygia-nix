# Nix copies paths to the store when `+` starts with a string, but not when
# it starts with a set that coerces to a string.
let
  f = <nix/fetchurl.nix>;
in
[
  ("a" + f)
  ({ outPath = "a"; } + f)
  ({ outPath = "a"; } + { outPath = f; })
  ({ __toString = s: f; } + "b")
  (builtins.getContext ({ outPath = "a"; } + f))
  (builtins.getContext ({ outPath = "a"; } + "${f}"))
  ({ outPath = "/a/"; } + "/../b//")
]
