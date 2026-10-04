# Attributes are forced in the order Nix first interned their names. Nix
# interns the variables of its `derivation` before reading any input, so
# `x` comes before `a` here.
builtins.tryEval (builtins.deepSeq { a = throw "a"; x = abort "x"; } null)
