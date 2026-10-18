let deep = leaf: n: builtins.foldl' (acc: _: [ acc ]) leaf (builtins.genList (x: x) n); in
builtins.genericClosure {
  startSet = [
    { key = deep 0 9997; }
    { key = deep 1 9997; }
  ];
  operator = _: [ ];
}
