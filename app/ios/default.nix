{ lib
, buildXcodeApp
, version
, buildNumber
, gitRevision
}:

buildXcodeApp {
  pname = "ogygia-ios";
  inherit version buildNumber gitRevision;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Ogygia
      ./Ogygia.xcodeproj
    ];
  };

  project = "Ogygia.xcodeproj";
  scheme = "Ogygia";
  # Any iOS or iPadOS device; the simulator is never built for.
  xcodebuildFlags = [ "-sdk" "iphoneos" "-destination" "generic/platform=iOS" ];

  installPhase = ''
    runHook preInstall

    mkdir -p "$out/Applications"
    cp -R "$derivedData/Build/Products/Release-iphoneos/Ogygia.app" "$out/Applications/Ogygia.app"

    runHook postInstall
  '';

  # Nothing in the bundle needs fixing up, and the release job signs it
  # exactly as xcodebuild left it.
  dontFixup = true;

  meta.description = "Ogygia app for iOS and iPadOS";
}
