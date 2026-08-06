#!/usr/bin/env bash
# Empaqueta TerminalCanvas como .app de macOS (Ship-it 7.1, T1).
#
# Uso:
#   scripts/bundle.sh                 # arma dist/TerminalCanvas.app
#   scripts/bundle.sh --dmg           # además arma el .dmg
#   CODESIGN_IDENTITY="Developer ID Application: Tu Nombre (TEAMID)" \
#     scripts/bundle.sh --dmg         # firma (T2; requiere tu Developer ID)
#
# La notarización queda documentada al final: necesita credenciales tuyas.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

APP_NAME="TerminalCanvas"
BUNDLE_ID="com.terminalcanvas.app"
BINARY_NAME="mi-terminal"
DIST_DIR="dist"
APP_DIR="$DIST_DIR/$APP_NAME.app"
MAKE_DMG=false
[[ "${1:-}" == "--dmg" ]] && MAKE_DMG=true

VERSION="$(awk -F'"' '/^version[[:space:]]*=/ {print $2; exit}' Cargo.toml)"
if [[ -z "$VERSION" ]]; then
  echo "No se pudo leer la versión de Cargo.toml" >&2
  exit 1
fi
echo "== $APP_NAME $VERSION =="

echo "-- build release"
cargo build --release --locked

echo "-- estructura del bundle"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "target/release/$BINARY_NAME" "$APP_DIR/Contents/MacOS/$APP_NAME"
chmod +x "$APP_DIR/Contents/MacOS/$APP_NAME"

echo "-- icono .icns"
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 64 128 256 512; do
  sips -z $size $size assets/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  double=$((size * 2))
  sips -z $double $double assets/icon.png \
    --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP_DIR/Contents/Resources/$APP_NAME.icns"
rm -rf "$(dirname "$ICONSET")"

echo "-- Info.plist"
cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>$APP_NAME</string>
  <key>CFBundleExecutable</key><string>$APP_NAME</string>
  <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
  <key>CFBundleIconFile</key><string>$APP_NAME</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <!-- Sin esto el texto del terminal se ve borroso en pantallas Retina. -->
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

if [[ -n "${CODESIGN_IDENTITY:-}" ]]; then
  echo "-- firma (hardened runtime)"
  codesign --force --deep --options runtime --timestamp \
    --sign "$CODESIGN_IDENTITY" "$APP_DIR"
  codesign --verify --deep --strict --verbose=2 "$APP_DIR"
else
  echo "-- sin CODESIGN_IDENTITY: bundle sin firmar (Gatekeeper lo va a bloquear)"
fi

if $MAKE_DMG; then
  echo "-- dmg"
  DMG="$DIST_DIR/$APP_NAME-$VERSION.dmg"
  rm -f "$DMG"
  STAGE="$(mktemp -d)"
  cp -R "$APP_DIR" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
  rm -rf "$STAGE"
  shasum -a 256 "$DMG" | tee "$DMG.sha256"
  echo "dmg: $DMG"
fi

echo
echo "OK: $APP_DIR"
cat <<'NOTES'

Notarización (T2, requiere tu Developer ID):

  xcrun notarytool submit dist/TerminalCanvas-<version>.dmg \
    --apple-id "tu@correo" --team-id "TEAMID" --password "app-specific-password" \
    --wait
  xcrun stapler staple dist/TerminalCanvas-<version>.dmg
NOTES
