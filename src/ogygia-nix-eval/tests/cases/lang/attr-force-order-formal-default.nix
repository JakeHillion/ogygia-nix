# Nix interns a formal's name after its default, so `ee` comes before `dd`
# here.
let
  g = { dd ? { ee = 1; } }: null;
in
builtins.tryEval (builtins.deepSeq { dd = abort "d"; ee = throw "e"; } null)
