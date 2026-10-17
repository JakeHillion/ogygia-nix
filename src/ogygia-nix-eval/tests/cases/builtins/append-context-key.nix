let
  f = builtins.unsafeDiscardStringContext (builtins.toFile "x" "y");
  dot = builtins.unsafeDiscardStringContext (builtins.toFile ".x" "y");
in
map
  (
    k:
    builtins.getContext (
      builtins.appendContext "x" {
        ${k} = {
          path = true;
          allOutputs = false;
          outputs = [ ];
        };
      }
    )
  )
  [
    "${f}/"
    "/${builtins.replaceStrings [ "/nix/store/" ] [ "/nix/store/./" ] f}/sub/.."
    dot
  ]
