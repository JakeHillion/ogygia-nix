let
  f = builtins.findFile [ ] "nix/fetchurl.nix";
in
[
  <nix/missing>
  <nix/.>
  f
  (dirOf f)
  (dirOf <nix/.>)
  (baseNameOf f)
  (toString f)
  (toString <nix/a/../b>)
  (f + "/x")
  (/a + f)
  (f == <nix/fetchurl.nix>)
  (f == /fetchurl.nix)
  (/z < f)
  (f < /a)
  (/a < f)
  (<nix/a> < <nix/b>)
  (builtins.typeOf f)
  (builtins.pathExists f)
  (builtins.pathExists <nix/missing>)
  (builtins.pathExists <nix/fetchurl.nix/x>)
  (builtins.readDir <nix/.>)
  (builtins.readFileType f)
  (builtins.readFileType <nix/.>)
  (builtins.hashFile "sha256" f)
  (builtins.stringLength (builtins.readFile f))
  "${f}"
  "${<nix/.>}"
  (builtins.path {
    path = <nix/.>;
    filter = p: t: p == "/fetchurl.nix";
  })
  (builtins.filterSource (p: t: false) <nix/.>)
  (builtins.toJSON f)
  (builtins.toXML <nix/missing>)
  ((import f) {
    url = "http://example.org/a";
    hash = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
  }).drvPath
]
