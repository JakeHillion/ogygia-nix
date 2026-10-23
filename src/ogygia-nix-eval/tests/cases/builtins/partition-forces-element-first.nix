# `partition` forces each element before calling the predicate on it.
[
  (builtins.tryEval (builtins.partition (a: "x") [ (throw "t") ]))
  (builtins.tryEval (builtins.partition (a: { }.b) [ (throw "t") ]))
]
