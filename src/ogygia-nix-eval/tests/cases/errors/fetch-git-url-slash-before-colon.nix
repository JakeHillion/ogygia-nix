builtins.tryEval (fetchGit {
  url = "./a:b";
  rev = throw "x";
})
