[
  (builtins.split "a*" "baaac")
  (builtins.split "" "ab")
  (builtins.split "x*" "")
  (builtins.split "(a)|b" "xaybz")
  (builtins.split "b*" "abbbc")
]
