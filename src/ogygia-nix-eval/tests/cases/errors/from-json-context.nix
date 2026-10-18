# The text fromJSON parses is a string without context.
builtins.fromJSON (builtins.toJSON (builtins.toFile "a" "b"))
