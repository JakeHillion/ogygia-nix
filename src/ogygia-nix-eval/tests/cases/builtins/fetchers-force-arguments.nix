let
  caught = e: !(builtins.tryEval e).success;
in
map caught [
  (fetchGit <key>)
  (fetchGit (throw "x"))
  (fetchGit { url = throw "x"; })
  (fetchGit {
    url = "a";
    name = throw "x";
  })
  (fetchGit {
    url = "/a";
    rev = throw "x";
  })
  (fetchGit {
    url = "file:a";
    rev = throw "x";
  })
  (fetchGit {
    url = "a:b";
    rev = throw "x";
  })
  (fetchGit {
    url = "git+https:";
    rev = throw "x";
  })
  (fetchGit {
    url = 1;
    rev = throw "x";
  })
  (fetchTarball (throw "x"))
  (fetchTarball { url = throw "x"; })
  (fetchTarball {
    url = "a";
    sha256 = throw "x";
  })
  (builtins.fetchurl (throw "x"))
  (builtins.fetchurl {
    url = "a";
    name = throw "x";
  })
  (fetchMercurial <key>)
  (fetchMercurial { url = throw "x"; })
  (fetchMercurial {
    url = "a";
    rev = throw "x";
  })
]
