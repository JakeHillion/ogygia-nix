let
  drv = derivation {
    name = "x";
    system = "x86_64-linux";
    builder = "/bin/sh";
  };
in
[
  (builtins.toXML builtins.toXML)
  (builtins.toXML (builtins.add 1))
  (builtins.toXML {
    b = [ 1 2.5 1.0e20 0.1 "x" null true false ];
    a = { c = "<d>\n\"&'\t\r"; };
    p = /foo/bar;
  })
  (builtins.toXML [ (x: x) ({ b, a ? 1, ... }: a) (args@{ c }: c) ({ }: 1) ])
  (builtins.toXML { outPath = "x"; })
  (builtins.toXML { type = "derivation"; })
  (builtins.toXML { type = "derivation"; drvPath = 1; outPath = "o"; })
  (builtins.toXML [ drv drv ])
  (builtins.getContext (builtins.toXML [ "${drv}" ]))
  (builtins.getContext (builtins.toXML drv))
  (builtins.toXML "")
]
