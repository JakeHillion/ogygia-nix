builtins.appendContext "x" {
  ${builtins.unsafeDiscardStringContext (builtins.toFile "a" "b")} = {
    outputs = [ "out" ];
  };
}
