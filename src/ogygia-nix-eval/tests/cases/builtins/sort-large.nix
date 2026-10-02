let
  xs = builtins.genList (i: { k = builtins.bitAnd (i * 7919) 31; i = i; }) 200;
in
map (x: x.i) (builtins.sort (a: b: a.k < b.k) xs)
