# A URL with a known scheme must be valid RFC 3986 syntax.
builtins.tryEval (fetchGit {
  url = "file:a =";
  rev = throw "x";
})
