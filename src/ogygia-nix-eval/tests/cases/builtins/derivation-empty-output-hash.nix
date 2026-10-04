# An empty `outputHash` stands for the all-zero hash of `outputHashAlgo`.
map (outputHashAlgo: (derivation { name = "d"; system = "x"; builder = "/bin/sh"; outputHash = ""; inherit outputHashAlgo; }).drvPath) [ "sha256" "md5" ]
