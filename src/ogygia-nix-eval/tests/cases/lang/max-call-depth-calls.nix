let deep = n: builtins.foldl' (acc: _: { y = acc; }) null (builtins.genList (x: x) n); bottom = s: if s ? y then bottom s.y else 0; in
bottom (deep 9997)
