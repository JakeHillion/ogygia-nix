map (xs: builtins.tryEval (builtins.length (builtins.sort (a: b: true) xs))) [
  [ (throw "x") ]
  [
    1
    (throw "x")
  ]
]
