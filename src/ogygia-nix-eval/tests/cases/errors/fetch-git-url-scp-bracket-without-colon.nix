# A host starting with '[' that is closed by ']' must be followed by ':'
# to be SCP-style; otherwise the URL is parsed as a URL.
builtins.tryEval (fetchGit {
  url = "[ toJSON:a]";
  rev = throw "x";
})
