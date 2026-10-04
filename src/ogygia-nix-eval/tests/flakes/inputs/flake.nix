{
  inputs.dep.url = "path:./dep";
  inputs.raw = {
    url = "path:./raw";
    flake = false;
  };
  inputs.nested.follows = "dep/nested";

  outputs = { self, dep, raw, nested }: {
    fromDep = dep.value;
    depSelf = dep.selfValue;
    depKeys = builtins.attrNames dep;
    depInputKeys = builtins.attrNames dep.inputs;
    rawKeys = builtins.attrNames raw;
    rawText = builtins.readFile "${raw}/text";
    rawInfo = builtins.attrNames raw.sourceInfo;
    nestedValue = nested.value;
    sameNested = nested.value == dep.inputs.nested.value;
    inputKeys = builtins.attrNames self.inputs;
    rawOutPath = raw.outPath;
    depOutPath = dep.outPath;
  };
}
