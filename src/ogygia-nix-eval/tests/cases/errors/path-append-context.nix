# A string with context cannot be appended to a path.
/a + builtins.toFile "a" "b"
