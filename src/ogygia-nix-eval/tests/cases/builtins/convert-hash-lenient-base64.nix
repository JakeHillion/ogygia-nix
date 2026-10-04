let
  convert = hash: builtins.convertHash { inherit hash; toHashFormat = "base16"; };
  convertSha256 = hash: builtins.convertHash { inherit hash; hashAlgo = "sha256"; toHashFormat = "base16"; };
in
[
  (convert "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF2=")
  (convert "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF2")
  (convert "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF2==")
  (convert "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF2=x!")
  (convert "sha256-47DEQpj8HBSa+/TImW+5\nJCeuQeRkm5NMpJWZG3hSuF2=")
  (convertSha256 "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuF2=")
  (convertSha256 "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSu\nF2")
]
