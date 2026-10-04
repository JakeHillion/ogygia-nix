# `<nix/fetchurl.nix>`: a fixed-output derivation fetched by Nix's built-in
# `builtin:fetchurl` builder. Written to reproduce the attributes Nix's own
# version produces.
{ system ? "builtin"
, url
, outputHash ? ""
, outputHashAlgo ? ""
, md5 ? ""
, sha1 ? ""
, sha256 ? ""
, sha512 ? ""
, hash ? ""
, executable ? false
, unpack ? false
, name ? baseNameOf (toString url)
, impure ? false
,
}:
let
  hashAttrs =
    if hash != "" then
      { outputHash = hash; outputHashAlgo = ""; }
    else if sha512 != "" then
      { outputHash = sha512; outputHashAlgo = "sha512"; }
    else if sha256 != "" then
      { outputHash = sha256; outputHashAlgo = "sha256"; }
    else if sha1 != "" then
      { outputHash = sha1; outputHashAlgo = "sha1"; }
    else if md5 != "" then
      { outputHash = md5; outputHashAlgo = "md5"; }
    else
      { inherit outputHash outputHashAlgo; };
in
derivation (
  {
    builder = "builtin:fetchurl";
    outputHashMode = if unpack || executable then "recursive" else "flat";
    inherit name url executable unpack;
    system = "builtin";
    preferLocalBuild = true;
    impureEnvVars = [ "http_proxy" "https_proxy" "ftp_proxy" "all_proxy" "no_proxy" ];
    urls = [ url ];
  }
    // (if impure then { __impure = true; } else hashAttrs)
)
