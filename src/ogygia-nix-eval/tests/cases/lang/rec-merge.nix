let a = 5; in [
  { x = rec { a = 1; b = c; }; x.c = 2; }
  { x = rec { a = 1; }; x.b = a; }
  { x = { a = 1; }; x = { b = 2; }; }
  { x = { a = 1; }; x.d = rec { e = a; }; }
]
