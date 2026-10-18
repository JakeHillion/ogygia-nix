# `e.a.${n}` evaluates each name before forcing the value it selects from,
# while `e ? a.${n}` forces each set before evaluating the name.
[
  (builtins.tryEval { a = abort "a"; }.a.${throw "b"})
  (builtins.tryEval ({ a = abort "a"; }.a.${throw "b"} or 1))
  (builtins.tryEval { a.b = abort "a"; }.a.b.${throw "b"})
  (builtins.tryEval ((throw "a") ? ${abort "b"}))
  (builtins.tryEval ({ a = throw "a"; } ? a.${abort "b"}))
  (builtins.tryEval (with { }; (throw "a") ? ${b}))
]
