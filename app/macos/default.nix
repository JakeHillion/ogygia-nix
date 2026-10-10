{ lib
, stdenvNoCC
, fetchurl
, unzip
, writeText
, xcode
, version
, buildNumber
, gitRevision
}:

let
  # Xcode resolves Swift packages over the network, which the sandbox
  # forbids. Instead, read the pins Xcode recorded in Package.resolved and
  # lay the packages out the way a resolve leaves them, so xcodebuild finds
  # everything already in place and never fetches.
  resolved = lib.importJSON ./Ogygia.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved;

  # Binary targets are downloaded from URLs that each package declares in
  # its own way, so they are described here per package identity. Both the
  # URL and the checksum are read from the manifest at the pinned revision,
  # so a pin bump in Xcode cannot leave them stale.
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

  packages = map
    (pin:
      let
        src = builtins.fetchGit {
          url = pin.location;
          rev = pin.state.revision;
          allRefs = true;
          shallow = true;
        };
      in
      {
        inherit pin src;
        name = lib.removeSuffix ".git" (baseNameOf pin.location);
        packageRef = {
          inherit (pin) identity kind location;
          name = lib.removeSuffix ".git" (baseNameOf pin.location);
        };
        artifacts = map
          (target: target // {
            # SwiftPM checksums are the SHA-256 of the archive, which is
            # exactly what fetchurl verifies.
            archive = fetchurl { inherit (target) url; sha256 = target.checksum; };
          })
          ((binaryTargets.${pin.identity} or (_: [ ])) src);
      })
    resolved.pins;

  # SwiftPM's workspace state (format 7, as written by Xcode 27), with
  # @SOURCE_PACKAGES@ standing in for the build-time directory.
  workspaceState = writeText "workspace-state.json" (builtins.toJSON {
    version = 7;
    object = {
      dependencies = map
        (p: {
          inherit (p) packageRef;
          state = {
            name = "sourceControlCheckout";
            # Absent fields are omitted, as Xcode writes them.
            checkoutState = lib.filterAttrs (_: v: v != null) {
              inherit (p.pin.state) revision;
              version = p.pin.state.version or null;
              branch = p.pin.state.branch or null;
            };
          };
          subpath = p.name;
          # SwiftPM requires the key, and discards the whole state without it.
          basedOn = null;
        })
        packages;
      artifacts = lib.concatMap
        (p: map
          (a: {
            inherit (p) packageRef;
            inherit (a) targetName;
            source = { type = "remote"; inherit (a) url checksum; };
            path = "@SOURCE_PACKAGES@/artifacts/${p.pin.identity}/${a.targetName}/${a.targetName}.xcframework";
            kind.xcframework = { };
          })
          p.artifacts)
        packages;
      prebuilts = [ ];
    };
  });

  layOutPackages = lib.concatMapStrings
    (p: ''
      cp -R ${p.src} "$sourcePackages/checkouts/${p.name}"
    '' + lib.concatMapStrings
      (a: ''
        mkdir -p "$sourcePackages/artifacts/${p.pin.identity}/${a.targetName}"
        unzip -q ${a.archive} -d "$sourcePackages/artifacts/${p.pin.identity}/${a.targetName}"
      '')
      p.artifacts)
    packages;
in
stdenvNoCC.mkDerivation {
  pname = "ogygia-macos";
  inherit version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Ogygia
      ./Ogygia.xcodeproj
    ];
  };

  nativeBuildInputs = [ unzip ];

  # Host paths xcodebuild cannot run without. They are readable only, and
  # the sandbox still has no network:
  # - /System/Library and /usr/lib: system frameworks and libraries.
  # - /usr/share/firmlinks: FSEvents maps paths between the system and data
  #   volumes with it; without it Xcode's file watcher gets no paths back
  #   and xcodebuild crashes.
  # - /Library/Developer/PrivateFrameworks: CoreSimulator and friends,
  #   installed by `xcodebuild -runFirstLaunch` and loaded by Xcode from
  #   that absolute path.
  # - /Library/Apple/System/Library/PrivateFrameworks: MobileDevice and
  #   friends, also installed at first launch, which CoreDevice loads
  #   through symlinks in /System/Library/PrivateFrameworks.
  # - /Library/Preferences/com.apple.dt.Xcode.plist: records acceptance of
  #   the Xcode licence, which xcodebuild checks.
  # - /usr/bin/codesign: Xcode verifies the signature of binary
  #   xcframeworks such as Sparkle's before linking them.
  # - /usr/bin/touch: the build system stamps bundles with it.
  # Builders must list these in allowed-impure-host-deps.
  __impureHostDeps = [
    "/System/Library"
    "/usr/lib"
    "/usr/share/firmlinks"
    "/Library/Developer/PrivateFrameworks"
    "/Library/Apple/System/Library/PrivateFrameworks"
    "/Library/Preferences/com.apple.dt.Xcode.plist"
    "/usr/bin/codesign"
    "/usr/bin/touch"
  ];

  configurePhase = ''
    runHook preConfigure

    # Foundation takes the home directory from the build user's account,
    # /var/empty, unless CFFIXED_USER_HOME overrides it.
    export HOME="$TMPDIR/home"
    export CFFIXED_USER_HOME="$HOME"
    mkdir -p "$HOME"
    # SwiftPM compiles package manifests without HOME, so the clang module
    # cache would otherwise land in /var/empty too.
    export XDG_CACHE_HOME="$TMPDIR/cache"
    export SWIFTPM_MODULECACHE_OVERRIDE="$TMPDIR/cache/ModuleCache"
    export DEVELOPER_DIR=${xcode}/Contents/Developer
    export PATH="$DEVELOPER_DIR/usr/bin:$PATH"

    sourcePackages="$TMPDIR/SourcePackages"
    mkdir -p "$sourcePackages/checkouts" "$sourcePackages/artifacts"
    ${layOutPackages}
    chmod -R u+w "$sourcePackages"
    sed "s|@SOURCE_PACKAGES@|$sourcePackages|g" ${workspaceState} > "$sourcePackages/workspace-state.json"

    runHook postConfigure
  '';

  buildPhase = ''
    runHook preBuild

    # Signing happens after the build, outside the sandbox, with the
    # Developer ID identity; the linker's ad-hoc signature is enough to run
    # the result locally.
    #
    # A process inside the Nix sandbox cannot apply a sandbox of its own,
    # so SwiftPM's manifest sandbox and the compiler's macro plugin sandbox
    # are turned off; both still run inside this one.
    xcodebuild \
      -project Ogygia.xcodeproj \
      -scheme Ogygia \
      -configuration Release \
      -derivedDataPath "$TMPDIR/DerivedData" \
      -clonedSourcePackagesDirPath "$sourcePackages" \
      -disableAutomaticPackageResolution \
      -onlyUsePackageVersionsFromResolvedFile \
      -skipPackagePluginValidation \
      -skipMacroValidation \
      -IDEPackageSupportDisableManifestSandbox=YES \
      build \
      CODE_SIGNING_ALLOWED=NO \
      OTHER_SWIFT_FLAGS='$(inherited) -disable-sandbox' \
      MARKETING_VERSION=${version} \
      CURRENT_PROJECT_VERSION=${toString buildNumber} \
      OGYGIA_GIT_REVISION=${gitRevision}

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p "$out/Applications"
    cp -R "$TMPDIR/DerivedData/Build/Products/Release/Ogygia.app" "$out/Applications/"

    runHook postInstall
  '';

  # Fixup would strip and rewrite binaries inside Sparkle.framework, which
  # Sparkle ships already signed.
  dontFixup = true;

  # Xcode's licence forbids redistributing it, so the app must never retain
  # a reference that would carry Xcode into a binary cache with it.
  disallowedReferences = [ xcode ];

  passthru = { inherit xcode; };

  meta = {
    description = "Ogygia menu bar app for macOS";
    license = lib.licenses.mit;
    platforms = [ "aarch64-darwin" ];
  };
}
