map (v: [ (builtins.typeOf v) (builtins.isAttrs v) (builtins.isList v) (builtins.isFunction v) (builtins.isString v) (builtins.isInt v) (builtins.isFloat v) (builtins.isBool v) (builtins.isNull v) (builtins.isPath v) ])
  [ 1 1.0 "s" ./. null true [ ] { } (x: x) builtins.map (builtins.map (x: x)) ]
