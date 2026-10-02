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
    packages = builtins.listToAttrs (map
      (system: {
        name = system;
        value = {
          pkg = "from packages";
          shadowed = "packages wins";
        };
      }) [ "x86_64-linux" "aarch64-linux" ]);
    legacyPackages = builtins.listToAttrs (map
      (system: {
        name = system;
        value.legacy = "from legacyPackages";
      }) [ "x86_64-linux" "aarch64-linux" ]);
    shadowed = "outputs loses";
    nested.attr."with.dot" = 1;
  };
}
