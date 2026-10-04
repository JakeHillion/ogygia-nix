# A comparator that is not a strict weak ordering still gives Nix's order.
builtins.sort (a: b: builtins.bitAnd a 3 < builtins.bitAnd b 5) (builtins.genList (i: i) 40)
