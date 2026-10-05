# Keys are deduplicated by Nix's ordering, not by equality.
let
  inf = 1.0e308 * 10;
  nan = inf - inf;
  count =
    ks:
    builtins.length (
      builtins.genericClosure {
        startSet = map (key: { inherit key; }) ks;
        operator = _: [ ];
      }
    );
in
map count [
  [ builtins.split ]
  [ { } ]
  [ 1 1.0 2 ]
  [ nan 1 2 ]
  [ 1 2 3 nan 4 ]
  [ 5 3 nan 8 1 4 7 9 2 6 0 nan 3 8 ]
  [ [ 1 ] [ 1 "a" ] [ 2 ] ]
  [ 5 3 8 1 4 7 9 2 6 0 3 8 ]
  [ "b" "a" "c" "a" ]
]
