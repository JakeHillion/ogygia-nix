# The operator is forced, and checked to be a function, before the first
# element of a non-empty startSet, so the throw in it wins over the abort.
builtins.tryEval (builtins.genericClosure {
  startSet = [ { key = abort "k"; } ];
  operator = throw "op";
})
