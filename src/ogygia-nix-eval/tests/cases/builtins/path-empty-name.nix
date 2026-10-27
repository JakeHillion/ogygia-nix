[
  (builtins.path { path = ../fixtures; name = ""; })
  (builtins.path { path = ../fixtures; name = ""; filter = p: t: baseNameOf p != "x"; })
]
