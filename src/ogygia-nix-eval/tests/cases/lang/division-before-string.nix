let
  x = {
    a = 6;
    b = 3;
  };
in
[
  ({ a = 6; }.a/(2))
  (x.a/(x.b))
  (builtins.isFunction (y: y.a/"${y.b}"))
  "${toString (x.a/(x.b))}"
  (x.a/(2) + __curPos.column)
  (6/(3))
]
