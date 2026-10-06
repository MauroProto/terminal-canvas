# Paquetes portables

Descargá el archivo de tu sistema y arquitectura junto con su `.sha256` desde
las releases oficiales de TerminalCanvas. Los nombres incluyen versión,
sistema y arquitectura (`x86_64` para Intel/AMD de 64 bits; `aarch64` para ARM
de 64 bits). Un paquete sin arquitectura no se considera universal.

La distribución configurada prepara Windows x86_64 (ZIP e instalador), Linux
x86_64 y macOS x86_64/aarch64. Los scripts portables aceptan también targets
ARM para builds manuales con el toolchain y linker correspondientes; su
presencia en el script no implica que haya un release publicado para esa
combinación. Los artefactos `dry-run-*` de Actions sirven para pruebas y no
tienen firma del proveedor. La validación y publicación de las primeras
versiones firmadas siguen pendientes; ver [RELEASE.md](RELEASE.md).

## Windows

Verificá el SHA-256 del ZIP con `Get-FileHash .\TerminalCanvas-<version>-windows-x86_64.zip -Algorithm SHA256`
y comparalo con el archivo `.sha256`. Extraé toda la carpeta y ejecutá
`mi-terminal.exe`. Conservá `tc-memory.exe` y `tc-memory-mcp.exe` al lado de
la aplicación. Se requiere Windows 10 1809 o posterior para ConPTY. No hace
falta instalar un servicio; el ZIP no instala accesos directos ni reemplaza
versiones anteriores.

El archivo `TerminalCanvas-<version>-windows-x86_64-setup.exe` instala por
usuario en `%LOCALAPPDATA%\Programs\TerminalCanvas` y agrega un acceso de
Inicio, un acceso de Escritorio opcional y un desinstalador. Compará también
su SHA256 y verificá con `Get-AuthenticodeSignature` que la firma sea válida.
Los releases reales exigen la misma firma de código para instalador, app,
helpers y desinstalador. Cerrá TerminalCanvas y sus terminales antes de
actualizar; el instalador no fuerza su cierre.

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
TerminalCanvas.app a `/Applications`; los helpers quedan dentro del bundle.

## Comprobar una carpeta extraída

Ejecutá `mi-terminal.exe --health-check` en Windows, `./mi-terminal
--health-check` en Linux o
`/Applications/TerminalCanvas.app/Contents/MacOS/TerminalCanvas --health-check`
en macOS. Informa versión, arquitectura, perfil y presencia de helpers sin
abrir una ventana ni iniciar terminales. Un helper faltante indica que hay
que extraer o reinstalar el paquete completo.

Para pruebas sin usar el perfil habitual, definí `TERMINAL_CANVAS_HOME` con
una carpeta absoluta propia antes de abrir la app. La app, sus helpers,
configuración, memoria y caché usan esa carpeta. No se modifica `HOME` ni la
configuración global de agentes. Un valor relativo o con `..` se rechaza;
no vuelve silenciosamente al perfil normal. Conservá esa carpeta si contiene
datos que querés seguir usando.

## Actualización y datos

El comprobador anuncia versiones estables y selecciona únicamente descargas
con versión, sistema y arquitectura coincidentes. Descarga en segundo plano,
muestra progreso, permite cancelar/reintentar y exige el checksum antes de
dejar listo el paquete. La instalación requiere una acción explícita y cerrar
las sesiones activas.

En Windows, una app actualmente firmada puede abrir el setup con una firma
válida del mismo certificado. La app sin firma descarga el ZIP para instalación
manual; el primer paso a una versión firmada se realiza manualmente. En macOS,
la app instalada en `/Applications` verifica notarización y el mismo Team ID,
y conserva una copia de la versión anterior al reemplazarla. Hay que cerrar
y volver a abrir la aplicación. En Linux, abrí la carpeta de descarga y
reemplazá manualmente el portable con la app cerrada.

Los paquetes no incluyen ni migran configuraciones, tokens ni la base de memoria
del usuario. Conservá tus datos de aplicación al reemplazar la carpeta del
programa. El desinstalador Windows quita los archivos y accesos del programa;
no borra el perfil de datos.

Para soporte, indicá versión, sistema/arquitectura, backend de terminal y pasos
para reproducir. Revisá cualquier diagnóstico antes de compartirlo y adjuntá
solamente lo necesario para el problema. El checksum detecta cambios en el
archivo; no reemplaza la firma del proveedor ni convierte un ZIP en instalador.
