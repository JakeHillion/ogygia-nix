[
  "${./fixtures/x}"
  (builtins.toFile "hello.txt" "hello world")
  (builtins.path { path = ./fixtures; name = "fx"; })
  (builtins.path { path = ./fixtures; filter = p: t: baseNameOf p != "x"; })
  (builtins.filterSource (p: t: true) ./fixtures)
  "${./fixtures}"
]
