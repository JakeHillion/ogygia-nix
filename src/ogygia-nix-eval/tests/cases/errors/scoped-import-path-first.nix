# The path is coerced before the scope is forced.
builtins.tryEval (builtins.scopedImport <ogygia-nix-eval-missing> { })
