# The index is forced before the list.
builtins.tryEval (builtins.elemAt (throw "list") (fromTOML "a"))
