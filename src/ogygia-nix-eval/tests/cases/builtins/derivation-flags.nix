let
  base = { name = "flags"; system = "x86_64-linux"; builder = "/bin/sh"; x = null; };
  paths = attrs: (derivation (base // attrs)).drvPath;
in
[
  (paths { })
  (paths { __structuredAttrs = false; })
  (paths { __structuredAttrs = true; })
  (paths { __ignoreNulls = false; })
  (paths { __ignoreNulls = true; })
  (paths { __ignoreNulls = true; __structuredAttrs = false; })
]
