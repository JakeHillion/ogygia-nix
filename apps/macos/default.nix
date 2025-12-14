{ pkgs }:

# NOTE: This is a placeholder derivation that demonstrates the app structure.
# Actual Swift compilation via Nix requires significant setup and is beyond
# the scope of this initial implementation. For now, this creates a stub
# .app bundle that can be replaced with a real build later.

pkgs.stdenv.mkDerivation {
  pname = "ogygia-macos";
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
    mkdir -p $out/Applications/Ogygia.app/Contents/MacOS
    mkdir -p $out/Applications/Ogygia.app/Contents/Resources

    # Create a stub executable (placeholder for actual Swift build)
    cat > $out/Applications/Ogygia.app/Contents/MacOS/Ogygia <<'EOF'
#!/bin/sh
echo "Ogygia macOS app (stub - requires Swift toolchain to build properly)"
echo "Source files are available in: $PWD"
EOF
    chmod +x $out/Applications/Ogygia.app/Contents/MacOS/Ogygia

    # Create Info.plist
    cat > $out/Applications/Ogygia.app/Contents/Info.plist <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>Ogygia</string>
    <key>CFBundleIdentifier</key>
    <string>com.ogygia.macos</string>
    <key>CFBundleName</key>
    <string>Ogygia</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
EOF

    # Copy source files for reference
    cp -r ${./.}/* $out/Applications/Ogygia.app/Contents/Resources/ || true
  '';

  meta = with pkgs.lib; {
    description = "Ogygia macOS menu bar app (stub build)";
    platforms = platforms.darwin;
  };
}
