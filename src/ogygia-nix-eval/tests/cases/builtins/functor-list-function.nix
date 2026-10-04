let f = { __functor = self: x: x > 1; }; in
[
  (builtins.genList f 3)
  (builtins.filter f [ 1 2 3 ])
  (builtins.partition f [ 1 2 3 ])
]
