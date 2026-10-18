let deep = n: builtins.foldl' (acc: _: { y = acc; }) null (builtins.genList (x: x) n); in
deep 9998 == deep 9998
