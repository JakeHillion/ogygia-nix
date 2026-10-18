let
  x = {
    a.b = 6;
    a.c = 3;
  };
in
[
  (x.${"a"}.b/(2))
  (x.${"a"}.b/(x.a.c))
  (builtins.isFunction (y: y.${"a"}.b/"${y.c}"))
  "${toString (x.${"a"}.b/(2))}"
  (x.a.${"b"}/(2))
]
