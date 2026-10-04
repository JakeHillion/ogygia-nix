map builtins.fromTOML [
  "a = 1 [b]\nc = 2"
  "a = [ 0.5,\n] [b]"
  "a = \"s\"[b]\nc = 1"
  "a = { x = 1 }[[b]]\nc = 3\n[[b]]"
  "a = 1.5 [b.c]"
  "a.b = 1 [x]"
]
