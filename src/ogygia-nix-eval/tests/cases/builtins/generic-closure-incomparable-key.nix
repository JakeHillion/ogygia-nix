# Keys are kept in an ordered set, so a later key that cannot be ordered
# against an earlier one is an error.
builtins.genericClosure {
  startSet = [
    { key = builtins.split; }
    { key = 2; }
  ];
  operator = _: [ ];
}
