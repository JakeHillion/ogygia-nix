[
  (builtins.match "[é-f]" "a")
  (builtins.match "[é-f]" "z")
  (builtins.match "[é-f]+" "é")
  (builtins.match "[é-f]+" "ü")
  (builtins.match "[à-é]+" "è")
  (builtins.split "[😎-f]" "a😎z")
]
