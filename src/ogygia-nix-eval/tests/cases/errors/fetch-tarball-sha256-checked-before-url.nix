# The hash is checked as soon as sha256 is reached, before url is forced, and
# tryEval cannot catch a bad hash.
builtins.tryEval (fetchTarball {
  sha256 = "a";
  url = throw "x";
})
