# A URL containing "://" anywhere is parsed as a URL, not as SCP-style.
builtins.tryEval (fetchGit {
  url = "a:b c://d";
  rev = throw "x";
})
