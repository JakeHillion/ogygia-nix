{ pkgs }:

# NOTE: This is a placeholder derivation that demonstrates the app structure.
# Actual Swift compilation via Nix requires significant setup and is beyond
# the scope of this initial implementation. For now, this creates a stub
# .app bundle that can be replaced with a real build later.

pkgs.stdenv.mkDerivation {
  pname = "ogygia-ios";
  version = "0.1.0";

  src = pkgs.lib.fileset.toSource {
    root = ./.;
    fileset = pkgs.lib.fileset.unions [
      ./Package.swift
      ./Sources
    ];
  };

  dontBuild = true;

  installPhase = ''
    mkdir -p $out/Applications/OgygiaIOS.app/Contents/MacOS
    mkdir -p $out/Applications/OgygiaIOS.app/Contents/Resources

    # Create a stub executable (placeholder for actual Swift build)
    cat > $out/Applications/OgygiaIOS.app/Contents/MacOS/OgygiaIOS <<'EOF'
#!/bin/sh
echo "Ogygia iOS app (stub - requires Swift toolchain and iOS SDK to build properly)"
echo "Source files are available in: $PWD"
EOF
    chmod +x $out/Applications/OgygiaIOS.app/Contents/MacOS/OgygiaIOS

    # Create Info.plist
    cat > $out/Applications/OgygiaIOS.app/Contents/Info.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>OgygiaIOS</string>
    <key>CFBundleIdentifier</key>
    <string>com.ogygia.ios</string>
    <key>CFBundleName</key>
    <string>OgygiaIOS</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSRequiresIPhoneOS</key>
    <true/>
    <key>UIRequiredDeviceCapabilities</key>
    <array>
        <string>arm64</string>
    </array>
</dict>
</plist>
EOF

    # Copy source files for reference
    cp -r ${./.}/* $out/Applications/OgygiaIOS.app/Contents/Resources/ || true
  '';

  meta = with pkgs.lib; {
    description = "Ogygia iOS app (stub build)";
    platforms = platforms.darwin;
  };
}
