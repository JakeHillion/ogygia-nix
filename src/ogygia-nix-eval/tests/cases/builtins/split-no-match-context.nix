let
  f = builtins.toFile "split-ctx" "contents";
in
[
  (builtins.getContext (builtins.head (builtins.split "," "${f}")))
  (builtins.getContext (builtins.head (builtins.split "/" "${f}")))
]
