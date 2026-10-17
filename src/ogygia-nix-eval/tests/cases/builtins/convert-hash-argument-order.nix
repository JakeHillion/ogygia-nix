# hashAlgo is forced and checked before toHashFormat is looked up, and
# toHashFormat before the hash itself is parsed.
let
  h = "0mdqa9w1p6cmli6976v4wi0sw9r4p5xrkzl7fjd1877wk11c9c46";
  caught = e: !(builtins.tryEval e).success;
in
map caught [
  (builtins.convertHash {
    hash = h;
    hashAlgo = throw "x";
  })
  (builtins.convertHash {
    hash = "md5:zz";
    hashAlgo = "sha256";
    toHashFormat = throw "x";
  })
  (builtins.convertHash {
    hash = "zz";
    toHashFormat = throw "x";
  })
]
