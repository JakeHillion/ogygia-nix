# The URL is checked when fetchGit reaches it, before later arguments.
builtins.tryEval (fetchGit {
  url = "a";
  rev = throw "x";
})
