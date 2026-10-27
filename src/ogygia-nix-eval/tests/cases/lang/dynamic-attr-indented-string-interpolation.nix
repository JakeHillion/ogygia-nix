# An indented string whose only piece is an interpolation of a literal
# name is that name, so the attribute is static: it merges with another
# definition of the name, is in scope in a recursive set, and may be bound
# by `let` and `inherit`. A `"` string interpolating it stays dynamic.
let
  x = 1;
  ${''${"y"}''} = 2;
in
[
  { ${''${"a"}''} = { b = 1; }; ${''${"a"}''} = { c = 1; }; }
  { a.b = 1; ${''${("a")}''} = { c = 1; }; }
  { ${''${''${''a''}''}''}.b = 1; a = { c = 1; }; }
  { ${''
      ${"a"}''} = { b = 1; }; ${''  ${''''}''} = { c = 1; }; "" = { d = 1; }; }
  (rec { ${''${"a"}''} = 1; b = a; })
  { inherit ${''${"x"}''}; }
  y
]
