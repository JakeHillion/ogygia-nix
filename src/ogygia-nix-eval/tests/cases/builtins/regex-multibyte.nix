[
  (builtins.match "." "é")
  (builtins.match ".." "é")
  (builtins.match "[é]" "é")
  (builtins.match "[é]+" "é")
  (builtins.match "[^a]" "é")
  (builtins.match "(.)(.)" "é")
  (builtins.match "é*" "éé")
  (builtins.split "." "é")
  (builtins.split " .( )" "a – b")
]
