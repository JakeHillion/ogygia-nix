# A derivation name is a string without context.
(derivation { name = baseNameOf (builtins.toFile "x" "y"); system = "x"; builder = "/bin/sh"; }).drvPath
