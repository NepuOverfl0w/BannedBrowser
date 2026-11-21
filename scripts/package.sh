#!/usr/bin/env bash
# Build release artifacts into dist/:
#   Linux:   bannedbrowser-<ver>-linux-x86_64.tar.gz (binary, .desktop, icons)
#   Windows: bannedbrowser-<ver>-windows-x86_64.zip (.exe, .ico)
#   macOS:   bannedbrowser-<ver>-macos.zip (BannedBrowser.app)
# TARGET=x86_64-pc-windows-gnu on Linux cross-builds the Windows zip.
set -euo pipefail
cd "$(dirname "$0")/.."
VER=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')
mkdir -p dist
OS="${TARGET:-$(uname -s)}"
case "$OS" in
  Linux)
    cargo build --release -p bannedbrowser
    D=dist/bannedbrowser-$VER-linux-x86_64
    rm -rf "$D"; mkdir -p "$D/share/applications" "$D/share/icons/hicolor"
    cp target/release/bannedbrowser "$D/"
    cp packaging/linux/bannedbrowser.desktop "$D/share/applications/"
    for s in 16 32 48 64 128 256 512; do
      mkdir -p "$D/share/icons/hicolor/${s}x${s}/apps"
      cp resources/icons/bannedbrowser-$s.png "$D/share/icons/hicolor/${s}x${s}/apps/bannedbrowser.png"
    done
    cp README.md LICENSE-MIT LICENSE-APACHE "$D/"
    tar -C dist -czf "$D.tar.gz" "$(basename "$D")"
    echo "$D.tar.gz" ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT|x86_64-pc-windows-gnu)
    T=""; [ "$OS" = "x86_64-pc-windows-gnu" ] && T="--target x86_64-pc-windows-gnu"
    cargo build --release -p bannedbrowser $T
    EXE=$(ls target/*/release/bannedbrowser.exe target/release/bannedbrowser.exe 2>/dev/null | head -1)
    D=dist/bannedbrowser-$VER-windows-x86_64
    rm -rf "$D"; mkdir -p "$D"
    cp "$EXE" resources/icons/bannedbrowser.ico README.md LICENSE-MIT LICENSE-APACHE "$D/"
    # TODO(M11): embed the icon and version info as a Windows resource.
    if command -v zip >/dev/null; then (cd dist && zip -qr "$(basename "$D").zip" "$(basename "$D")"); else powershell -Command "Compress-Archive -Force -Path '$D' -DestinationPath '$D.zip'"; fi
    echo "$D.zip" ;;
  Darwin)
    cargo build --release -p bannedbrowser
    APP=dist/BannedBrowser.app
    rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    cp target/release/bannedbrowser "$APP/Contents/MacOS/"
    ICONSET=$(mktemp -d)/BannedBrowser.iconset; mkdir -p "$ICONSET"
    for s in 16 32 128 256 512; do
      cp resources/icons/bannedbrowser-$s.png "$ICONSET/icon_${s}x${s}.png"
      d=$((s*2)); [ -f resources/icons/bannedbrowser-$d.png ] && cp resources/icons/bannedbrowser-$d.png "$ICONSET/icon_${s}x${s}@2x.png"
    done
    iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/BannedBrowser.icns" || true
    cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>BannedBrowser</string>
<key>CFBundleIdentifier</key><string>io.github.bannedbrowser</string>
<key>CFBundleVersion</key><string>$VER</string>
<key>CFBundleShortVersionString</key><string>$VER</string>
<key>CFBundleExecutable</key><string>bannedbrowser</string>
<key>CFBundleIconFile</key><string>BannedBrowser</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
    (cd dist && zip -qr "bannedbrowser-$VER-macos.zip" BannedBrowser.app)
    echo "dist/bannedbrowser-$VER-macos.zip" ;;
  *) echo "unsupported OS $OS" >&2; exit 1 ;;
esac
