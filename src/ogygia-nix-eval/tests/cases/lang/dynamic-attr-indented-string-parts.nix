# An indented string the lexer reads as several pieces, such as text and
# an escape, is a dynamic attribute name even without an interpolation, so
# a recursive set does not bring it into scope. Pieces that are empty once
# indentation is stripped do not count.
let
  ab = 0;
  a = 0;
in
[
  (rec { ${''a''\b''} = 1; x = ab; })
  (rec { ${''''\a''\b''} = 1; x = ab; })
  (rec { ${(''a''\b'')} = 1; x = ab; })
  (rec { ${''a''\b''}.c = 1; x = ab; })
  (rec { ${''''\ab''} = 1; x = ab; })
  (rec { ${'' ''\a''} = 1; x = a; })
]
