builtins.tryEval (fetchGit {
  url = "https://h:65536/";
  rev = throw "x";
})
