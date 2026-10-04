# Nix interns the names it refers to itself in a fixed order, `outPath`
# before `__functor`.
builtins.tryEval (builtins.deepSeq { __functor = throw "f"; outPath = abort "o"; } null)
