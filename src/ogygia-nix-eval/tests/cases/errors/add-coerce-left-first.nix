# The left operand of `+` is coerced to a string before the right is evaluated.
builtins.tryEval ({ } + throw "b")
