{
  description = "Equivalence fixture without inputs";

  outputs = { self }: {
    files = builtins.attrNames (builtins.readDir ./.);
    subFiles = builtins.readDir ./sub;
    data = import ./data.nix;
    selfKeys = builtins.attrNames self;
    outputKeys = builtins.attrNames self.outputs;
    hasRev = self ? rev;
    info = builtins.removeAttrs self.sourceInfo [ "outPath" ];
    outPathIsStore = builtins.match "/nix/store/[0-9a-z]{32}-source" self.outPath != null;
    selfOutPath = self.outPath;
    type = self._type;
    copied = "${./data.nix}";
    packages.x86_64-linux.pkg = "from packages";
    packages.x86_64-linux.shadowed = "packages wins";
    legacyPackages.x86_64-linux.legacy = "from legacyPackages";
    shadowed = "outputs loses";
    nested.attr."with.dot" = 1;
  };
}
