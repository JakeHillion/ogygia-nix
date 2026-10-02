[
  ((x: x + 1) 1)
  (({ a, b ? a + 1, ... }@args: [ a b args ]) { a = 1; c = 2; })
  (builtins.functionArgs ({ a, b ? 1 }: a))
  ((args@{ a }: args) { a = 2; })
]
