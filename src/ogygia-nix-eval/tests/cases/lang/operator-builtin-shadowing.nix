# Nix desugars `-`, `*`, `/`, unary minus and the comparisons to calls of
# `__sub`, `__mul`, `__div` and `__lessThan`, which lexical bindings shadow
# but `with` does not.
[
  (let __sub = a: b: [ a b ]; in 1 - 2)
  (let __sub = a: b: [ a b ]; in -5)
  (let __mul = a: b: [ a b ]; in 2 * 3)
  (let __div = a: b: [ a b ]; in 6 / 3)
  (let __lessThan = a: b: [ a b ]; in 1 < 2)
  (let __lessThan = a: b: [ a b ]; in 1 > 2)
  (let __lessThan = a: b: true; in 1 <= 2)
  (let __lessThan = a: b: true; in 1 >= 2)
  (({ __sub }: 3 - 1) { __sub = a: b: [ a b ]; })
  (rec { __mul = a: b: "m"; x = 2 * 3; }).x
  (with { __sub = a: b: [ a b ]; }; 1 - 2)
]
