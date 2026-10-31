let
  f = x: x;
  g = builtins.add 1;
in
[
  (builtins.elem fromTOML [ fromTOML ])
  (builtins.elem builtins.fromTOML [ builtins.fromTOML ])
  (builtins.elem f [ 1 f ])
  (builtins.elem g [ g ])
  (builtins.elem f [ (if true then f else f) ])
]
