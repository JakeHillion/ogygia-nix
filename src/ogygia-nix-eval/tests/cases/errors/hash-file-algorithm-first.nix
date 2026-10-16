# The algorithm is checked before the path is forced.
builtins.tryEval (builtins.hashFile "x" (throw "x"))
