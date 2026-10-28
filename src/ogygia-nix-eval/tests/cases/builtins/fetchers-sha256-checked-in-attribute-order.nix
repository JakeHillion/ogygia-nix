# A bad sha256 only fails once it is reached in attribute order, and an empty
# one is not checked.
let
  caught = e: !(builtins.tryEval e).success;
in
map caught [
  (fetchTarball {
    url = throw "x";
    sha256 = "a";
  })
  (fetchTarball {
    sha256 = "";
    url = throw "x";
  })
  (fetchTarball {
    sha256 = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    url = throw "x";
  })
  (builtins.fetchurl {
    name = throw "x";
    sha256 = "a";
  })
  (builtins.fetchurl {
    sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
    url = throw "x";
  })
]
