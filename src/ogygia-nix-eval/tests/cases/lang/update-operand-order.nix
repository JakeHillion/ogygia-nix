# `a // b` evaluates `b` and checks that it is a set before evaluating `a`.
[
  (builtins.tryEval ((abort "a") // (throw "b")))
  (builtins.tryEval (1 // (throw "b")))
]
