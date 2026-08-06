# Release

## 1. Empaquetar

```sh
scripts/bundle.sh --dmg
```

Arma `dist/TerminalCanvas.app` y `dist/TerminalCanvas-<version>.dmg`, con el
`.icns` generado desde `assets/icon.png` y un `Info.plist` con
`CFBundleIdentifier`, `NSHighResolutionCapable` y la versión leída de
`Cargo.toml`.

## 2. Firmar y notarizar (requiere Developer ID)

```sh
export CODESIGN_IDENTITY="Developer ID Application: Tu Nombre (TEAMID)"
scripts/bundle.sh --dmg

xcrun notarytool submit dist/TerminalCanvas-<version>.dmg \
  --apple-id "tu@correo" --team-id "TEAMID" \
  --password "app-specific-password" --wait
xcrun stapler staple dist/TerminalCanvas-<version>.dmg
```

Sin firma, Gatekeeper bloquea la app en cualquier máquina que no sea la que la
compiló. La app **verifica la firma del bundle descargado antes de instalarlo**
(`src/update_install.rs`): si el release no está firmado, la autoactualización
lo rechaza a propósito.

## 3. Publicar

1. Subir el `.dmg` y su `.sha256` al release de GitHub.
2. Actualizar `version` y `sha256` en `packaging/terminalcanvas.rb` y pushear
   el cask al tap.

## 4. Verificar

```sh
brew install --cask OWNER/tap/terminalcanvas
open /Applications/TerminalCanvas.app
```
