# A context that cannot be coerced to a string, or whose evaluation fails,
# does not replace the error it was added to, so `tryEval` still catches it.
[
  (builtins.tryEval (builtins.addErrorContext 1 (throw "x")))
  (builtins.tryEval (builtins.addErrorContext (abort "c") (throw "x")))
  (builtins.tryEval (builtins.addErrorContext 1 (assert false; 1)))
]
