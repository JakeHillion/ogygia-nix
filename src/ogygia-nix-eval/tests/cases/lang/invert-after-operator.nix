# `!` can follow an operator that binds tighter than it, and its operand then
# extends over every operator that also binds tighter.
map builtins.isFunction [
  (x: x + !x)
  (x: x - !x * x)
  (x: x * !x + x)
  (x: x / !x ++ x)
  (x: x ++ !x ? y)
  (x: -!x)
  (x: x + !!x + !x)
  (x: x + !-x x)
  (x: x + !x.y or x)
  (x: x + !{ a = 1; }.a == x)
  (x: x + !x // x)
  (x: x + !x < x && x)
  (x: x + !./a/${x}/b + "${x + !x}")
  (x: x + !let { body = x; })
]
