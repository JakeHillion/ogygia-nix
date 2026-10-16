# The algorithm is checked before the string is forced.
builtins.tryEval (builtins.hashString "x" (throw "x"))
