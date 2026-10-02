[
  (({ a ? b, b ? true }: a) { })
  (({ a ? b, b ? true }: a) { b = false; })
  (({ a ? args, ... }@args: builtins.attrNames a) { c = 1; })
  (({ a ? b + 1, b ? 1 }: [ a b ]) { })
]
