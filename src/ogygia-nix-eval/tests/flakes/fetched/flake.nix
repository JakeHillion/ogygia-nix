{
  inputs.tarball.url = "file://@WORK@/tarball.tar.gz";
  inputs.git.url = "git+file://@WORK@/git";
  inputs.raw = {
    url = "file://@WORK@/raw.tar.gz";
    flake = false;
  };

  outputs = { self, tarball, git, raw }: {
    fromTarball = tarball.value;
    fromGit = git.value;
    tarballInfo = builtins.removeAttrs tarball.sourceInfo [ "outPath" ];
    gitInfo = builtins.removeAttrs git.sourceInfo [ "outPath" ];
    rawInfo = builtins.removeAttrs raw [ "outPath" "sourceInfo" ];
    tarballOutPath = tarball.outPath;
    gitOutPath = git.outPath;
    rawOutPath = raw.outPath;
    rawText = builtins.readFile "${raw}/text";
    rawFiles = builtins.readDir raw;
    rawSub = builtins.readDir "${raw}/sub";
    tool = builtins.readFile "${tarball}/bin/tool";
    gitFiles = builtins.readDir git;
    gitLink = builtins.readFileType "${git}/link";
  };
}
