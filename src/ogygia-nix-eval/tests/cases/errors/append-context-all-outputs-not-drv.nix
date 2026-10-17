builtins.appendContext "x" {
  ${builtins.unsafeDiscardStringContext (builtins.toFile "a" "b")} = {
    allOutputs = true;
  };
}
