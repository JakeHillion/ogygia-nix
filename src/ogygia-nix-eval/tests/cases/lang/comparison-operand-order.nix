# `a > b` and `a <= b` evaluate `b` before `a`.
[
  (builtins.tryEval ((abort "a") > (throw "b")))
  (builtins.tryEval ((abort "a") <= (throw "b")))
  (builtins.tryEval ((throw "a") < (abort "b")))
  (builtins.tryEval ((throw "a") >= (abort "b")))
]
