# The path is coerced before the filter is forced.
builtins.tryEval (builtins.filterSource (throw "filter") "relative")
