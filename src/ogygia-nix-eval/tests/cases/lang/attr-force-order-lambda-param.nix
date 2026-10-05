# Nix interns a function's parameter name after its body, so `qq` comes
# before `pp` here.
let
  f = pp: qq: null;
in
builtins.tryEval (builtins.deepSeq { pp = abort "p"; qq = throw "q"; } null)
