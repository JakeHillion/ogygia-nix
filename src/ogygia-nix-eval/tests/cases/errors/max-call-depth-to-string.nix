let deep = leaf: n: builtins.foldl' (acc: _: [ acc ]) leaf (builtins.genList (x: x) n); in
builtins.toString (deep 0 9997)
