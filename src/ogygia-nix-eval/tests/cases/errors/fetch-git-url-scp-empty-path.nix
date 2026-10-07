builtins.tryEval (fetchGit {
  url = "git+rsync:";
  rev = throw "x";
})
