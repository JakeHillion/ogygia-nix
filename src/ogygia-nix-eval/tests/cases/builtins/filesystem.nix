[
  (builtins.readFile ./fixtures/x)
  (builtins.readDir ./fixtures)
  (builtins.pathExists ./fixtures/x)
  (builtins.pathExists ./fixtures/nope)
  (builtins.readFileType ./fixtures/x)
  (builtins.readFileType ./fixtures)
  (builtins.hashFile "sha256" ./fixtures/x)
  (import ./fixtures/value.nix)
  (import ./fixtures)
  (builtins.scopedImport { x = 5; } ./fixtures/scoped.nix)
]
