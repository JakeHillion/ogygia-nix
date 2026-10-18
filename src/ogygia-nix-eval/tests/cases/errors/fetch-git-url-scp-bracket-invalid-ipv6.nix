# A bracketed SCP-style host must be an IPv6 address, without a zone.
builtins.tryEval (fetchGit {
  url = "[::1%25a]:b";
  rev = throw "x";
})
