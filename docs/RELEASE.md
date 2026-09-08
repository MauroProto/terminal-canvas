# Release

## Estado actual

El repositorio puede producir paquetes portables Windows/Linux y `.app`/DMG
para ambas arquitecturas macOS. El bundle de macOS
activa el feature `daemon`, incluye `mi-terminal-daemon`, `tc-memory` y
`tc-memory-mcp` junto a la app y firma primero esos helpers cuando se configura
una identidad. Un tag `v*` activa
`.github/workflows/release.yml`, que compila con Rust 1.98.0, exige firma y
notarización para macOS y verifica los cuatro paquetes antes de publicar juntos
sus artefactos y checksums. Los runners macOS son `macos-15-intel` y `macos-15`
(ARM64), con targets explícitos; Windows y Linux se publican para x86_64.
Las etiquetas de runners están documentadas por
[GitHub Actions](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

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

Ejecuta `cargo build --release --locked --features daemon --bins --target <target>`
y arma `dist/TerminalCanvas.app` y
`dist/TerminalCanvas-<version>-macos-<arch>.dmg`. El target predeterminado es
el host de rustc; `TC_BUNDLE_TARGET` acepta `x86_64-apple-darwin` o
`aarch64-apple-darwin` y `lipo` verifica la arquitectura de cada ejecutable.
No produce un bundle universal. La app contiene
el ejecutable principal y los tres helpers en `Contents/MacOS`, además del `.icns`
generado desde `assets/icon.png` y un `Info.plist` con `CFBundleIdentifier`,
`NSHighResolutionCapable` y la versión leída de `Cargo.toml`.

En Windows, `./scripts/package-portable.ps1` genera el ZIP x86_64; en Linux,
`scripts/package-portable.sh` genera el tar.gz del host. Ambos incluyen la app,
los helpers de memoria, licencia e instrucciones, y generan un checksum con
nombre de archivo relativo a la carpeta de descarga. El paquete Linux incluye
también el daemon; Windows usa ConPTY sin servicio separado. Ver
[PORTABLE.md](PORTABLE.md) para requisitos y actualización manual.

## 2. Firmar y notarizar (requiere Developer ID)

```sh
export CODESIGN_IDENTITY="Developer ID Application: Tu Nombre (TEAMID)"
scripts/bundle.sh --dmg

xcrun notarytool submit dist/TerminalCanvas-<version>-macos-<arch>.dmg \
  --apple-id "tu@correo" --team-id "TEAMID" \
  --password "app-specific-password" --wait
xcrun stapler staple dist/TerminalCanvas-<version>-macos-<arch>.dmg
# Regenerar el checksum después de staple, que modifica el DMG:
(cd dist && shasum -a 256 TerminalCanvas-<version>-macos-<arch>.dmg > TerminalCanvas-<version>-macos-<arch>.dmg.sha256)
```

Sin firma, Gatekeeper bloquea la app en cualquier máquina que no sea la que la
compiló. El script firma explícitamente los tres helpers antes del bundle de la
app. Existe verificación de firma para un DMG descargado en
`src/update_install.rs`, pero el flujo automático de descargar/instalar y su
endpoint todavía no están habilitados de punta a punta; no debe anunciarse como
autoactualización operativa.

## 3. Publicar (pendiente; requiere accesos externos)

1. Usar el workflow de release para generar y verificar los cuatro paquetes
   antes de publicarlos. Ningún job publica parcialmente si falla otra plataforma.
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
