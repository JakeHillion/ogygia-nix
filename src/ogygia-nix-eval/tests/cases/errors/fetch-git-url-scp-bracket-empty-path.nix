# The user of a bracketed SCP-style URL runs to its last '@'.
builtins.tryEval (fetchGit {
  url = "u@a:b@[::1]:";
  rev = throw "x";
})
