let deep = leaf: n: builtins.foldl' (acc: _: [ acc ]) leaf (builtins.genList (x: x) n); in
deep 0 9998 < deep 1 9998
