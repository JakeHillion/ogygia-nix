let deep = n: builtins.foldl' (acc: _: { y = acc; }) null (builtins.genList (x: x) n); in
builtins.toJSON (deep 9997)
