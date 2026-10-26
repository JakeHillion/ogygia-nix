{ lib
, stdenvNoCC
, unzip
, removeReferencesTo
, swiftPackages
, buildXcodeApp
, darwin
, iana-etc
, libiconv
, mailcap
, tzdata
, nebula
, ogygia-macos-nebulad
}:

let
  # Sparkle's binary target. Both the URL and the checksum are read from the
  # manifest at the pinned revision, so a pin bump in Xcode cannot leave
  # them stale.
  binaryTargets = {
    sparkle = src:
      let
        manifest = builtins.readFile "${src}/Package.swift";
        field = name:
          let m = builtins.match ''.*let ${name} = "([^"]+)".*'' manifest;
          in if m == null then throw "Sparkle's Package.swift no longer declares `let ${name}`" else lib.head m;
      in
      [{
        targetName = "Sparkle";
        url = builtins.replaceStrings [ "\\(tag)" ] [ (field "tag") ] (field "url");
        checksum = field "checksum";
      }];
  };

  packages = swiftPackages {
    resolved = ./Ogygia.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved;
    inherit binaryTargets;
  };

  # Sparkle's signing tools ship in the same archive as the framework, so
  # releases are signed by the version the app links against.
  sparkle = lib.findFirst (p: p.pin.identity == "sparkle") (throw "Package.resolved does not pin Sparkle") packages;
  sparkleTools = stdenvNoCC.mkDerivation {
    pname = "sparkle-tools";
    version = sparkle.pin.state.version;
    src = (lib.head sparkle.artifacts).archive;
    nativeBuildInputs = [ unzip ];
    sourceRoot = ".";
    installPhase = ''
      mkdir -p "$out"
      cp -R bin "$out/bin"
    '';
    # The tools are signed by the Sparkle project; leave them untouched.
    dontFixup = true;
  };

  # Executables bundled beside the app's own: the Nebula daemon, and the
  # Nebula it runs, at the nixpkgs pin.
  helpers = [
    "${ogygia-macos-nebulad}/bin/ogygia-macos-nebulad"
    "${nebula}/bin/nebula"
    "${nebula}/bin/nebula-cert"
  ];

  # The helpers load these from the store, but a Mac running the app has no
  # store paths, so they load the system's copies instead. Both are in the
  # dyld shared cache rather than on disk.
  systemLibraries = [
    { from = "${libiconv}/lib/libiconv.2.dylib"; to = "/usr/lib/libiconv.2.dylib"; }
    { from = "${darwin.libresolv}/lib/libresolv.9.dylib"; to = "/usr/lib/libresolv.9.dylib"; }
  ];

  # Go embeds these as the first places to look for its service, MIME and
  # time zone tables, and falls back to the system's copies when they are
  # missing, as they are on a Mac running the app.
  fallbackReferences = [ iana-etc mailcap tzdata ];
in
buildXcodeApp {
  pname = "ogygia-macos";
  inherit packages;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Ogygia
      ./Ogygia.xcodeproj
      ./LaunchDaemons
    ];
  };

  project = "Ogygia.xcodeproj";
  scheme = "Ogygia";

  nativeBuildInputs = [ removeReferencesTo ];

  installPhase = ''
    runHook preInstall

    mkdir -p "$out/Applications"
    app="$out/Applications/Ogygia.app"
    cp -R "$derivedData/Build/Products/Release/Ogygia.app" "$app"

    # SMAppService finds the daemon's plist here, and launchd runs the
    # daemon from Contents/MacOS.
    install -D -m 0644 LaunchDaemons/dev.ogygia.macos.nebulad.plist \
      "$app/Contents/Library/LaunchDaemons/dev.ogygia.macos.nebulad.plist"
    for helper in ${lib.escapeShellArgs helpers}; do
      bin="$app/Contents/MacOS/$(basename "$helper")"
      install -m 0755 "$helper" "$bin"
      ${lib.concatMapStrings (l: ''
        "$DEVELOPER_DIR/Toolchains/XcodeDefault.xctoolchain/usr/bin/install_name_tool" -change ${l.from} ${l.to} "$bin"
      '') systemLibraries}
      remove-references-to ${lib.concatMapStringsSep " " (p: "-t ${p}") fallbackReferences} "$bin"
      # Rewriting the binary invalidated the linker's ad-hoc signature, which
      # arm64 requires to run at all.
      /usr/bin/codesign --force --sign - "$bin"
    done

    runHook postInstall
  '';

  # Fixup would strip and rewrite binaries inside Sparkle.framework, which
  # Sparkle ships already signed.
  dontFixup = true;

  passthru = { inherit sparkleTools; };

  meta.description = "Ogygia menu bar app for macOS";
}
