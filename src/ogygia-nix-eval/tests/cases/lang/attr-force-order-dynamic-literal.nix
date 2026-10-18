# Nix's parser interns a name written as `${"lit"}` where it appears, like
# a static name, so `zr` comes before `zq` here.
builtins.tryEval (builtins.deepSeq { ${"zr"} = abort "r"; zq = throw "q"; } null)
