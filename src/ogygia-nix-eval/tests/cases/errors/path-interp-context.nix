# A string with context cannot be interpolated into a path.
/a/${builtins.toFile "a" "b"}
