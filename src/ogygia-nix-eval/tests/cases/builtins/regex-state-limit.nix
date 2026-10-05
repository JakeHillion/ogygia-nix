let
  repeat = n: s: builtins.concatStringsSep "" (builtins.genList (_: s) n);
in
[
  (builtins.match (repeat 99996 "a") "")
  (builtins.split (repeat 99996 "a") "")
  (builtins.match "((a|b)*){2}{3844}" "")
  (builtins.match "(a+|(b|c{2})*){3,4760}" "")
  (builtins.match (repeat 8333 "(a|b|)*") "")
]
