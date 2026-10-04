# An empty builder counts as a missing one, structured attrs or not.
(derivation { name = "d"; system = "x"; builder = ""; __structuredAttrs = true; }).drvPath
