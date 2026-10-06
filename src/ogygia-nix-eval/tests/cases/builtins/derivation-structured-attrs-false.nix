# `__structuredAttrs = false` stays in the environment, unlike `__ignoreNulls`.
(derivation { name = "d"; system = "x"; builder = "/bin/sh"; __structuredAttrs = false; }).drvPath
