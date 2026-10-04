# An empty system counts as a missing one.
(derivation { name = "d"; system = ""; builder = "/bin/sh"; }).drvPath
