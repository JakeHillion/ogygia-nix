[
  (builtins.match "(a?)??c" "c")
  (builtins.match "(a|)??" "")
  (builtins.match "(a)*?(a*)" "aa")
  (builtins.match "(a){1,2}?(a*)" "aa")
  (builtins.match "[a]+?(a*)" "aa")
  (builtins.match "(a)**" "aa")
  (builtins.split ">( ?|x)??" "a>")
  (builtins.split "(a)+?" "aa")
]
