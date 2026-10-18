# The contents are forced before the name is checked as a store path name.
builtins.tryEval (builtins.toFile "a b" (throw "contents"))
