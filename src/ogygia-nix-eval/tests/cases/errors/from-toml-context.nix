# The text fromTOML parses is a string without context.
builtins.fromTOML (builtins.substring 0 0 (builtins.toFile "a" "b") + "a = 1")
