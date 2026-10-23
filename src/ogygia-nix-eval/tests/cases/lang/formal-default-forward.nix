# A formal's default may refer to the `@` binding or to a formal listed
# after it.
[
  (({ a, b ? args, ... }@args: [ a b args ]) { a = 1; c = 2; })
  ((args@{ a, b ? args, ... }: [ a b args ]) { a = 1; c = 2; })
  (({ a ? b, b ? 1 }: [ a b ]) { })
  (({ a ? b, b }: [ a b ]) { b = 3; })
  (({ a ? b, b ? c, c ? args, ... }@args: a) { d = 4; })
]
