# `outputHashMode` is validated even when `outputHash` is not set.
(derivation { name = "f"; system = "x86_64-linux"; builder = "/bin/sh"; outputHashMode = "rcursive"; }).outPath
