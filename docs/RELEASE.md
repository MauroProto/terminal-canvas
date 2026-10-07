# Release

## Estado y alcance

La distribución prepara cinco archivos, cada uno con su `.sha256`:

| Plataforma | Artefacto |
| --- | --- |
| Windows x86_64 | `TerminalCanvas-<version>-windows-x86_64.zip` |
| Windows x86_64 | `TerminalCanvas-<version>-windows-x86_64-setup.exe` |
| Linux x86_64 | `TerminalCanvas-<version>-linux-x86_64.tar.gz` |
| macOS Intel | `TerminalCanvas-<version>-macos-x86_64.dmg` |
| macOS Apple Silicon | `TerminalCanvas-<version>-macos-aarch64.dmg` |

El workflow [release.yml](../.github/workflows/release.yml) tiene dos modos:

- `workflow_dispatch`: compila y verifica los paquetes en runners reales sin
  secretos de firma. Sube artefactos `dry-run-*` por 14 días y nunca publica.
- Push del tag exacto `v<version de Cargo.toml>`: exige firma Authenticode de
  Windows y Developer ID/notarización de macOS. Publica sólo cuando las cuatro
  plataformas y la verificación del conjunto completo hayan pasado.

El código y sus controles están preparados; esto no acredita un release firmado
ya ejecutado. El [ensayo base de GitHub](https://github.com/MauroProto/terminal-canvas/actions/runs/37545144493)
sobre `d79f0fa` terminó en verde: extracción del ZIP Windows, construcción,
instalación y desinstalación de Inno Setup en el runner, tarball Linux y ambos
DMG macOS montados y comprobados. Los helpers pasaron sus smoke tests y el
conjunto final contenía exactamente cinco paquetes y sus cinco checksums.
El job de publicación se omitió y no se crearon tags ni releases.
El [ensayo actualizado](https://github.com/MauroProto/terminal-canvas/actions/runs/37549143310)
sobre `5107f41` incluye las acciones Node 24 y el preflight ZIP; sus 27
regresiones Python y la corrida completa terminaron en verde: ZIP y setup Windows,
tarball Linux, ambos DMG, helpers y exactamente cinco paquetes con sus checksums.
También omitió publicación. La [continuación de entrega](DELIVERY-CONTINUATION.md)
registra los commits, la CI y los pendientes. Los cambios documentales posteriores
incluyen `PORTABLE.md`: para comprobar los archivos del ensayo contra sus fuentes,
usar el checkout `5107f41`. Una release real reconstruye desde el tag exacto.
Siguen pendientes las firmas/notarización
con credenciales reales y la publicación de una release firmada.
Los artefactos de Actions en este repositorio público son descargables por
lectores autenticados; los paquetes de ensayo no tienen firma del proveedor.
`packaging/terminalcanvas.rb` sigue siendo una plantilla
con checksums pendientes y su tap todavía debe publicarse. Las pruebas locales
de fixtures y sintaxis no sustituyen esas verificaciones externas.

Los runners son `windows-2025`, `ubuntu-24.04`, `macos-15-intel` y `macos-15`.
GitHub documenta sus [imágenes y arquitecturas](https://docs.github.com/en/actions/reference/runners/github-hosted-runners);
la [imagen Windows 2025](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)
incluye Inno Setup. Las etiquetas de runner no prometen compatibilidad del
paquete Linux con sistemas más antiguos.

## Empaquetado local

Los scripts de compilación exigen `rustup` y la versión exacta de
`rust-toolchain.toml`. Resuelven Cargo/rustc con `rustup which`, comprueban sus
versiones y usan esos ejecutables durante el proceso; no modifican el entorno
persistente. El verificador y el instalador requieren Python 3.11 o posterior
(el workflow fija Python 3.12). Windows necesita también Inno Setup 6 para
generar el instalador y el Windows SDK para firmarlo.

```powershell
# Windows: primero el portable, después un instalador con ese mismo contenido.
./scripts/package-portable.ps1
./scripts/package-installer.ps1
python scripts/package-verify.py --archive dist/TerminalCanvas-<version>-windows-x86_64.zip --target x86_64-pc-windows-msvc --smoke
```

`package-installer.ps1` valida y extrae el ZIP antes de compilar el instalador.
La instalación es por usuario en `%LOCALAPPDATA%\Programs\TerminalCanvas`, con
acceso directo de Inicio, acceso de Escritorio opcional y desinstalador. No
fuerza el cierre de aplicaciones ni reinicia sesiones. `-Compiler` permite
indicar la ruta a `ISCC.exe`. `-VerifyInstall` se limita a runners GitHub
efímeros: instala en una carpeta temporal, compara el contenido con el ZIP,
ejecuta las comprobaciones de la app/helpers y verifica la desinstalación.

```sh
# Linux: incluye el daemon de PTYs además de los helpers de memoria.
scripts/package-portable.sh
python scripts/package-verify.py --archive dist/TerminalCanvas-<version>-linux-x86_64.tar.gz --target x86_64-unknown-linux-gnu --smoke

# macOS: incluye la app, el daemon y ambos helpers en Contents/MacOS.
scripts/bundle.sh --dmg
python scripts/package-verify.py --dmg dist/TerminalCanvas-<version>-macos-<arch>.dmg --target <target-apple-darwin>
```

Los targets macOS son `x86_64-apple-darwin` y `aarch64-apple-darwin` mediante
`TC_BUNDLE_TARGET`; `lipo` comprueba los ejecutables. No se genera un bundle
universal. Los portables aceptan targets ARM manuales con su toolchain/linker,
pero el workflow distribuye únicamente Windows/Linux x86_64; el instalador
Windows también es x86_64. Ver [PORTABLE.md](PORTABLE.md) para uso y requisitos.

## Verificaciones y dry-run

Desde Actions → Release → Run workflow se puede ejecutar el dry-run de la rama.
No necesita certificados ni tiene una ruta habilitada para publicar. Los
artefactos de esa corrida son builds de prueba sin firma del proveedor y no
deben usarse para preparar un cask público.

El verificador comprueba los nombres/versiones, SHA256, el manifiesto completo,
la arquitectura de todos los binarios y los permisos ejecutables Unix. Rechaza
entradas adicionales, duplicadas, enlaces y rutas fuera de la carpeta del
portable. Los controles de `.app` se hacen sobre el DMG montado, incluyendo
metadatos de versión, icono, arquitectura y helpers. Los smoke tests ejecutan
`--version`, `--health-check`, `tc-memory --help`, `tc-memory health` y una
negociación MCP con listado de herramientas desde los archivos extraídos,
usando `TERMINAL_CANVAS_HOME` y una base de memoria temporales.

Antes de cargar el parser ZIP se limita el índice a 2 MiB y a las entradas
del manifiesto, con una carpeta raíz opcional. Se comprueban los índices reales,
los conteos declarados y los offsets, también en ZIP64; no alcanza con falsificar
el footer. Los TAR se recorren sólo hasta exceder el manifiesto. Las 27
regresiones pequeñas incluyen archivos válidos y metadatos falsificados. El
índice y la lista de entradas se validan antes de extraer; el contenido esperado,
la arquitectura y los smoke tests se comprueban después de extraer.

```sh
# Fixtures pequeños: no compilan Rust ni ejecutan instaladores.
python -B scripts/package-verify-tests.py
# Con los cinco archivos y sus cinco checksums descargados en una carpeta:
python scripts/package-verify.py --release-dir dist
```

La validación final exige exactamente esos diez archivos, sin versiones
anteriores mezcladas ni plataformas faltantes. El workflow también ejecuta
formato, Clippy y tests antes de empaquetar. Las pruebas gráficas y el uso de
terminales reales siguen requiriendo validación específica de cada sistema.

## Credenciales para publicar

Todos estos secretos deben ser reales y estar configurados en GitHub; su
ausencia impide el release, sin fallback a paquetes sin firma:

| Sistema | Secrets |
| --- | --- |
| macOS | `MACOS_CERTIFICATE_P12` (base64), `MACOS_CERTIFICATE_PASSWORD`, `MACOS_KEYCHAIN_PASSWORD`, `CODESIGN_IDENTITY`, `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` |
| Windows | `WINDOWS_CERTIFICATE_PFX` (base64), `WINDOWS_CERTIFICATE_PASSWORD`, `WINDOWS_TIMESTAMP_URL` |

El certificado macOS debe ser Developer ID Application. Se firman los helpers,
la app y el DMG; se exige respuesta `Accepted` de Apple, se hace staple y se
regenera el checksum porque staple modifica el archivo. La verificación exige
el Team ID configurado, hardened runtime, Gatekeeper y ticket de notarización.

Windows importa un certificado de firma de código vigente con clave privada
en `CurrentUser/My`. El mismo certificado firma la app, ambos helpers, el
instalador y el desinstalador con SHA256 y timestamp RFC3161 mediante la URL
HTTPS configurada. Cada firma debe resultar válida y coincidir con el
thumbprint importado. Los materiales de firma temporales se eliminan al
finalizar los jobs. Para firmar localmente, los scripts leen
`TC_WINDOWS_CERT_THUMBPRINT` y `TC_WINDOWS_TIMESTAMP_URL` después de importar
el certificado real; si no se configuran, la build local sirve para pruebas.

## Publicación y comprobación final

Revisar primero una corrida completa de dry-run. Después de configurar los
accesos y certificados, publicar el tag exacto de Cargo activa el release. Si
falla un job, no se publican parcialmente las plataformas restantes. El permiso
de escritura sólo se concede al job `publish`, habilitado para ese evento/tag.

Luego hay que descargar los artefactos publicados, verificar sus checksums,
probar instalación y lanzamiento en cada plataforma y comprobar sus firmas.
Para el tap, reemplazar ambos SHA256 y la versión de
`packaging/terminalcanvas.rb` por los de los DMG finales ya firmados y
notarizados, revisar el cask y publicarlo con acceso real. La publicación de
GitHub y la del tap son entregas distintas; no se debe afirmar que el tap está
disponible mientras la plantilla conserve placeholders.

El actualizador implementa descarga verificada y requiere una acción explícita
para instalar. macOS instala sólo desde la app válida de `/Applications`, con
el mismo Developer ID y respaldo de la versión anterior; Windows abre sólo un
setup firmado por el mismo certificado que la app actual y exige que el registro
de Inno identifique esa misma instalación. Las copias Windows portables, incluso
firmadas, y las apps sin firma utilizan ZIP y actualización manual; el primer
cambio a una versión firmada también es manual. Linux conserva la extracción
manual. El flujo real de actualización
con un release firmado también queda pendiente de validación externa.

Los helpers de verificación Windows se crean asociados a su Job privado antes
de ejecutar código del hijo. Se comprueba esa asociación mientras el proceso
está suspendido y un fallo aborta el lanzamiento. Tienen plazo y límite de
salida; el Job termina su árbol al finalizar o perder el guard, incluso ante
salida del padre sin `Drop`. La cancelación disponible corresponde a la descarga.
Los dry-runs históricos citados arriba preceden este cambio: comprobar la CI y
el ensayo de distribución del nuevo HEAD antes de publicar un candidato.
