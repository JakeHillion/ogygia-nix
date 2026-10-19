# The user of a bracketed SCP-style URL runs to its first '@', and the host
# to the ']' closing the first "@[".
builtins.tryEval (fetchGit {
  url = "u@a:b@[::1]:";
  rev = throw "x";
})
