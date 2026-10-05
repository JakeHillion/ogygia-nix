# Nix interns names as it parses them, so `b` in the inner set comes before
# the outer `c`.
builtins.tryEval (builtins.deepSeq { a = { b = throw "b"; c = abort "c"; }; c = 1; } null)
