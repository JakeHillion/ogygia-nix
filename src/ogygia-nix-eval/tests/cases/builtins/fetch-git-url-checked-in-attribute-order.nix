# Attributes are reached in the order their names were first read, so rev and
# foo come before url.
let
  caught = e: !(builtins.tryEval e).success;
in
map caught [
  (fetchGit {
    rev = throw "x";
    foo = 1;
    url = "a";
  })
  (fetchGit {
    foo = throw "x";
    url = "";
  })
]
