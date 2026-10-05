# Every element the operator returns is forced before any is checked to be
# a set, so the throw in a later element wins over the earlier integer.
builtins.tryEval (builtins.genericClosure {
  startSet = [ { key = 1; } ];
  operator = x: [ 3 (throw "t") ];
})
