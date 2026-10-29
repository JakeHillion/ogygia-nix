# Nix registers `fromTOML` among its last builtins, after `toJSON`, so
# `toJSON` is forced first here.
builtins.tryEval (builtins.deepSeq { fromTOML = throw "f"; toJSON = abort "t"; } null)
