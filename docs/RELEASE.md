# Release

## Estado actual

El repositorio puede producir una `.app` y un DMG locales. El bundle de macOS
activa el feature `daemon`, incluye `mi-terminal-daemon`, `tc-memory` y
`tc-memory-mcp` junto a la app y firma primero esos helpers cuando se configura
una identidad. Un tag `v*` activa
`.github/workflows/release.yml`, que exige firma, notariza, verifica y publica
el DMG y su checksum.

Esto **no equivale a un release público completo**. Siguen pendientes:

- firmar y notarizar con credenciales Apple reales;
- crear y publicar un release de GitHub con sus artefactos;
- generar el SHA256 de `packaging/terminalcanvas.rb` y publicar el cask en un
  tap real;
- completar la instalación automática del update. El checker consulta el
  repositorio real, informa releases y abre la descarga manual, pero no instala
  artefactos por sí solo.

## 1. Empaquetar

```sh
scripts/bundle.sh --dmg
```

Ejecuta `cargo build --release --locked --features daemon --bins` y arma
`dist/TerminalCanvas.app` y `dist/TerminalCanvas-<version>.dmg`. La app contiene
el ejecutable principal y los tres helpers en `Contents/MacOS`, además del `.icns`
generado desde `assets/icon.png` y un `Info.plist` con `CFBundleIdentifier`,
`NSHighResolutionCapable` y la versión leída de `Cargo.toml`.

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
compiló. El script firma explícitamente los tres helpers antes del bundle de la
app. Existe verificación de firma para un DMG descargado en
`src/update_install.rs`, pero el flujo automático de descargar/instalar y su
endpoint todavía no están habilitados de punta a punta; no debe anunciarse como
autoactualización operativa.

## 3. Publicar (pendiente; requiere accesos externos)

1. Subir el `.dmg` y su `.sha256` al release de GitHub.
2. Actualizar `version` y `sha256` en `packaging/terminalcanvas.rb` y pushear
   el cask al tap.

No se debe declarar el release publicado sin un tap, un SHA256 generado desde
el DMG final y acceso al repositorio de releases.

## 4. Verificar después de publicar

```sh
brew install --cask MauroProto/tap/terminalcanvas
open /Applications/TerminalCanvas.app
```

El tap todavía tiene que publicarse. La verificación final también debe
confirmar que el helper está presente y firmado:

```sh
test -x /Applications/TerminalCanvas.app/Contents/MacOS/mi-terminal-daemon
test -x /Applications/TerminalCanvas.app/Contents/MacOS/tc-memory
test -x /Applications/TerminalCanvas.app/Contents/MacOS/tc-memory-mcp
codesign --verify --deep --strict --verbose=2 \
  /Applications/TerminalCanvas.app
```
