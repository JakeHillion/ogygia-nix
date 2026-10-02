let
  f = builtins.toFile "ctx-test" "contents";
  s = "${f}/sub";
in
[
  (builtins.hasContext s)
  (builtins.hasContext "plain")
  (builtins.getContext s)
  (builtins.unsafeDiscardStringContext s)
  (builtins.hasContext (builtins.unsafeDiscardStringContext s))
  (builtins.getContext (builtins.appendContext "x" { "${builtins.unsafeDiscardStringContext f}" = { path = true; }; }))
]
