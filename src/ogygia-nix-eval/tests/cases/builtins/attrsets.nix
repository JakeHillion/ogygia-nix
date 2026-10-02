let s = { b = 2; a = 1; c = 3; }; in
[
  (builtins.attrNames s)
  (builtins.attrValues s)
  (builtins.getAttr "a" s)
  (builtins.hasAttr "d" s)
  (builtins.removeAttrs s [ "a" "z" ])
  (builtins.intersectAttrs { a = 0; c = 0; } s)
  (builtins.listToAttrs [ { name = "x"; value = 1; } { name = "x"; value = 2; } { name = "y"; value = 3; } ])
  (builtins.mapAttrs (n: v: n + toString v) s)
  (builtins.catAttrs "a" [ { a = 1; } { b = 2; } { a = 3; } ])
  (builtins.zipAttrsWith (n: vs: vs) [ { a = 1; } { a = 2; b = 3; } ])
  (builtins.unsafeGetAttrPos "a" s)
  (builtins.unsafeGetAttrPos "zz" s)
]
