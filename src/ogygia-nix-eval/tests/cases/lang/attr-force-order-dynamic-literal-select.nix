# A literal name in a selection or `?` path, even in parentheses, is
# interned where it appears, so `zr` comes before `zq` here.
let
  t = { } ? ${("zr")};
in
builtins.tryEval (builtins.deepSeq { zq = throw "q"; zr = abort "r"; } null)
