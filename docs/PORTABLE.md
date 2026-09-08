# Paquetes portables

Descargá el archivo de tu sistema y arquitectura junto con su `.sha256` desde
las releases oficiales de TerminalCanvas. Los nombres incluyen versión,
sistema y arquitectura (`x86_64` para Intel/AMD de 64 bits; `aarch64` para ARM
de 64 bits). Un paquete sin arquitectura no se considera universal.

La distribución configurada genera Windows x86_64, Linux x86_64 y macOS
x86_64/aarch64. Los scripts portables aceptan también targets ARM para builds
manuales con el toolchain y linker correspondientes; su presencia en el script
no implica que haya un release publicado para esa combinación.

## Windows

Verificá el SHA-256 del ZIP con `Get-FileHash .\TerminalCanvas-<version>-windows-x86_64.zip -Algorithm SHA256`
y comparalo con el archivo `.sha256`. Extraé toda la carpeta y ejecutá
`mi-terminal.exe`. Conservá `tc-memory.exe` y `tc-memory-mcp.exe` al lado de
la aplicación. No hace falta instalar un servicio; el backend de terminales
usa ConPTY. El ZIP no instala accesos directos ni reemplaza versiones anteriores.

## Linux

Verificá la descarga desde su carpeta con
`sha256sum -c TerminalCanvas-<version>-linux-x86_64.tar.gz.sha256`, extraé el
tar y ejecutá `./mi-terminal` dentro de la carpeta. Conservá los helpers
`mi-terminal-daemon`, `tc-memory` y `tc-memory-mcp` junto al ejecutable.
El paquete se compila en Ubuntu 24.04: requiere un sistema compatible con sus
bibliotecas nativas, una sesión gráfica y un driver gráfico compatible.
No es un binario estático ni promete compatibilidad con distribuciones más antiguas.

## macOS

Elegí `macos-aarch64.dmg` en Apple Silicon y `macos-x86_64.dmg` en Intel.
Verificá el checksum desde la carpeta de descarga con `shasum -a 256 -c <archivo>.sha256`.
Los DMG del workflow de release requieren firma Developer ID y notarización;
un bundle local sin credenciales no equivale a un release firmado. Copiá
TerminalCanvas.app a Aplicaciones; los helpers quedan dentro del bundle.

## Actualización y datos

El comprobador anuncia versiones y selecciona únicamente descargas con
versión, sistema y arquitectura coincidentes. La instalación automática sigue
sin implementarse: descargá, verificá y reemplazá la carpeta o app manualmente
con TerminalCanvas cerrado. Los paquetes no incluyen ni migran configuraciones,
tokens ni la base de memoria del usuario. Conservá tus datos de aplicación al
reemplazar la carpeta del programa.

Para soporte, indicá versión, sistema/arquitectura, backend de terminal y pasos
para reproducir. Revisá cualquier diagnóstico antes de compartirlo y adjuntá
solamente lo necesario para el problema. El checksum detecta cambios en el
archivo; no reemplaza la firma del proveedor ni convierte un ZIP en instalador.
