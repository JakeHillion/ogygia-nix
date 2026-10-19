let
  pairs = [
    [ 1 2 ]
    [ 2 2 ]
    [ 3 2 ]
    [ "a" "b" ]
    [ [ 1 3 ] [ 1 2 ] ]
  ];
in
map (p: {
  lt = builtins.elemAt p 0 < builtins.elemAt p 1;
  gt = builtins.elemAt p 0 > builtins.elemAt p 1;
  le = builtins.elemAt p 0 <= builtins.elemAt p 1;
  ge = builtins.elemAt p 0 >= builtins.elemAt p 1;
}) pairs
