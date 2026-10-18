let
  deep = n: builtins.foldl' (acc: _: { y = acc; }) null (builtins.genList (x: x) n);
  deepList = leaf: n: builtins.foldl' (acc: _: [ acc ]) leaf (builtins.genList (x: x) n);
in
builtins.deepSeq (deep 9996) true
&& deep 9997 == deep 9997
&& deepList 0 9997 < deepList 1 9997
&& builtins.toJSON (deep 9996) != ""
&& builtins.toXML (deep 9996) != ""
&& builtins.toString (deepList 0 9996) != ""
&& builtins.length (
  builtins.genericClosure {
    startSet = [
      { key = deepList 0 9996; }
      { key = deepList 1 9996; }
    ];
    operator = _: [ ];
  }
) == 2
