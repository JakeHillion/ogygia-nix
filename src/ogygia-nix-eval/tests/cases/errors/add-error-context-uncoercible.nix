# A context that cannot be coerced to a string replaces the error it was added
# to, so `tryEval` no longer catches it.
builtins.tryEval (builtins.addErrorContext 1 (throw "x"))
