# Soporte y recuperación

Para reportar un fallo usá [el formulario del repositorio](https://github.com/MauroProto/terminal-canvas/issues/new?template=bug_report.yml).
Incluí versión o commit, sistema y arquitectura, shell, agente/comando usado,
pasos mínimos y qué esperabas ver. Si aparece al reabrir, indicá si cerraste
la ventana normalmente, terminó el proceso o se reinició el equipo.

## Qué se conserva

| Dato | Comportamiento |
| --- | --- |
| Workspaces, paneles y divisiones | Se guardan en `layout.json`; la identidad de cada terminal y tarea acompaña al panel. |
| Historial local | Se restaura desde checkpoints y logs. El historial es finito y tiene límites de almacenamiento. |
| Procesos en Windows | ConPTY vive dentro de la app. Cerrar la app termina esos procesos; reabrir restaura el contexto guardado y crea terminales nuevas. |
| Procesos en macOS/Linux con daemon | Cerrar la ventana permite reconectarse al proceso del daemon. Cerrar explícitamente un panel termina esa sesión. Reiniciar el equipo termina los procesos. |
| Notas de revisión | Se guardan fuera del repositorio, con identidad del proyecto y revisión. Una nota de una revisión anterior sigue disponible en «Todas las notas». |
| Memoria de agentes | SQLite separa proyecto, rama y tarea. La memoria pendiente requiere revisión antes de compartirse como contexto. |
| Worktrees archivados | Archivar conserva archivos, incluidos cambios locales. Restaurar vuelve a registrarlos como worktree. |

La captura del estado del terminal conserva pantalla, cursor, modos y varias
propiedades de formato. No es una imagen de memoria del proceso ni una garantía
de restaurar cualquier estado privado de un emulador o aplicación TUI.

## Ubicación de los datos

La aplicación usa los directorios de configuración y datos de `terminal-app`
resueltos por el sistema. `config.toml` está en configuración; `layout.json`,
historial, notas y `memory/memory.db` están en datos. En Windows estos dos
directorios pueden ser distintos. `TC_MEMORY_DB` permite cambiar la ubicación
de la base de memoria. El log de panic está en `~/.mi-terminal/logs/panic.log`.

`mi-terminal --health-check` informa versión, plataforma, presencia de los
helpers y las rutas exactas de esta instalación sin abrir la interfaz, una
terminal ni la base de memoria. Si falta un helper, devuelve un error: extraé
el paquete completo antes de volver a probar. El informe contiene rutas locales;
revisalas antes de compartirlo.
No comprueba permisos de escritura ni el contenido de tus datos. Tampoco
informa las rutas efectivas de memoria o historial definidas por overrides;
revisá esas variables antes de preparar un respaldo.

## Probar con un perfil aislado

Para reproducir un problema sin abrir tus datos habituales, configurá
`TERMINAL_CANVAS_HOME` con una carpeta absoluta dedicada. La app, el daemon y
los helpers usan sus subcarpetas `config`, `data` y `cache`; todos los exports se
exportan a `exports` y el log de panic queda dentro de `data/logs`.
La app no instala ni elimina hooks en la configuración global de Claude en
este modo. No migra, copia ni elimina datos del perfil habitual.

```powershell
$env:TERMINAL_CANVAS_HOME = Join-Path $env:TEMP 'TerminalCanvas-prueba'
.\mi-terminal.exe --health-check
.\mi-terminal.exe
# En otra ejecución con la misma carpeta se restaura ese perfil.
```

En Unix: `TERMINAL_CANVAS_HOME=/tmp/tc-prueba ./mi-terminal`. Usá una ruta
corta si activás el daemon, porque los sockets Unix limitan su longitud.
Una ruta relativa, vacía o con `..` se rechaza antes de iniciar la app.
La variable dura sólo en el proceso/consola donde se define. Para volver al
perfil habitual, quitála del entorno. Los overrides específicos
`TC_MEMORY_DB`, `MI_TERMINAL_SCROLLBACK_DIR` y `MI_TERMINAL_DAEMON_DIR` conservan
su precedencia; quitálos también para una prueba completamente aislada.

Si aparece «No se pudo recuperar el historial», copiá primero la salida nueva
que necesitás conservar antes de cerrar o reiniciar. En ese caso el guardado
del historial queda pausado durante esta ejecución: la app conserva los archivos
anteriores y el marcador de recuperación, pero la salida nueva sigue en memoria.
Resolvé el bloqueo de lectura y reabrí la app para recuperar esos archivos.

Para hacer una copia antes de investigar un problema de persistencia, cerrá
las instancias de la app y los procesos que escriben sus datos. En Unix,
cerrar la ventana no detiene el daemon: finalizá sus sesiones y detenelo
también antes de copiar el historial. Guardá una copia completa de los
directorios de datos y configuración. Si copiás SQLite, incluí sus archivos `-wal`
y `-shm` cuando existan. Conservá la copia original para comparar resultados.
Si definiste `TC_MEMORY_DB` o `MI_TERMINAL_SCROLLBACK_DIR`, incluí además la
base de memoria y el directorio de historial de esas rutas: pueden quedar fuera
de los directorios del perfil. Copiar sólo `config` y `data` no los conserva.

Si aparece un aviso de que otra instancia tiene la persistencia, cerrá la
instancia anterior y reabrí la que vas a usar. La propiedad de escritura protege
el layout y el historial guardado por la UI frente a una instancia anterior;
la configuración, notas y SQLite tienen sus propios mecanismos de escritura.

Si aparece la franja «Guardado de layout e historial desactivado», pasá el
mouse sobre el motivo para ver el detalle. Puede indicar un archivo que no
se pudo leer, un formato de una versión más nueva o pérdida de la propiedad
de escritura. La franja permanece después de que desaparece el toast.
Los cambios de layout y la salida nueva de esa ejecución no quedan guardados
por la UI: exportá lo que necesites conservar y esperá la confirmación antes
de cerrar. Cerrá las instancias, respaldá los datos y resolvé el bloqueo de
lectura antes de reabrir; para un formato más nuevo, usá una versión compatible.
No borres el original ni reemplaces `layout.json` por un backup mientras no
se pueda leer. Un archivo ausente permite crear un perfil; un error de lectura
en un archivo existente detiene la recuperación automática desde backups.

El layout admite hasta 16 MiB de JSON completo en UTF-8, incluido el salto
final que agrega el guardado. El historial de terminales y las notas tienen
sus propios archivos y límites. Si la carga encuentra un layout o backup
que supera el máximo, lo trata como un archivo que no se pudo leer: conserva
los archivos y pausa el guardado de layout e historial, aunque exista otro
backup legible. Un layout válido no requiere leer los backups posteriores. No se
recorta ni se reemplaza automáticamente. Respaldá el perfil completo antes
de investigar su tamaño; no borres metadatos del original para ocultar el aviso.

Si falla una escritura por una entrada inválida en los backups, conservá
también las cinco entradas del ring al respaldar el perfil. Esos lugares
deben estar ausentes o ser archivos regulares directos; una carpeta, un
archivo especial o un enlace no se rota automáticamente.

## Notas de revisión y guardados pendientes

Una colección de notas admite hasta 4 MiB de JSON codificado en UTF-8, incluidos
campos, metadatos y formato. El mismo máximo se aplica a cargar notas, importar
un archivo legacy y guardar la colección. Importar un archivo que supera el
máximo devuelve un error; una fusión puede superar el máximo aunque cada
colección por separado entre. Un guardado rechazado conserva el archivo
anterior y muestra el motivo. El máximo no se calcula sólo por cantidad de
notas o caracteres.

Si cerraste el review después de un guardado fallido, reabrilo en el mismo
repositorio. Cuando aparezca **Editar notas pendientes**, esa acción recupera
el último snapshot aceptado que todavía está en memoria. Podés reducir o
corregir la colección y volver a guardarla. La acción no confirma guardado;
esperá el resultado antes de cerrar la app. **Reintentar guardado** vuelve a
intentar el contenido pendiente sin modificarlo, por lo que no resuelve por sí
solo una colección que excede el máximo.

La recuperación conserva IDs, revisión y estado de las notas, pero sólo dura
durante esa sesión. No incluye texto de un editor que nunca guardaste ni
garantiza recuperar un proceso terminado. Si no hay un snapshot pendiente del
mismo repositorio, la acción no aparece: un archivo ilegible o inválido sigue
bloqueando mutaciones hasta resolver su lectura. Conservá una copia del
archivo original antes de investigar o adaptar una colección demasiado grande;
no la trunques ni reemplaces por una colección vacía para ocultar el error.

## Diagnósticos

La exportación de diagnóstico recoge configuración, layout y logs con límites
de tamaño. Redacta campos estructurados sensibles y omite una configuración
que no se pueda interpretar con seguridad. Los mensajes de error y logs aún
pueden contener texto privado: revisá el ZIP antes de adjuntarlo a un issue.
No hace falta publicar la base de memoria ni el historial completo para
reportar un problema de interfaz.

Los diagnósticos y exports de texto se escriben en segundo plano. La app
acepta una exportación por vez e informa el resultado y su ruta cuando termina;
«Exportación en curso» sólo confirma que aceptó el trabajo. Cerrar normalmente
la app espera el trabajo aceptado. Una terminación forzada no ofrece esa barrera.
Los exports de texto incluyen un identificador único además de la fecha, para
conservar dos capturas del mismo panel hechas dentro del mismo segundo.

## Instalación y actualización

Consultá [PORTABLE.md](PORTABLE.md) para elegir arquitectura, verificar el
checksum y conservar los helpers junto a la app. El comprobador descarga el
paquete exacto y verifica su SHA256. La instalación automática exige una app
firmada instalada y el mismo editor; Windows usa un instalador Authenticode,
macOS exige Developer ID y notarización. Las sesiones deben estar cerradas y
los cambios guardados antes de instalar. Linux y builds sin firma requieren
extraer manualmente el paquete verificado. Los scripts y
workflows de empaquetado no significan que exista una release firmada publicada.

## Desarrollo en Windows

Si conviven Rust instalado por MSI y rustup, verificá que `cargo`, `rustc` y
`cargo-clippy` correspondan al toolchain de `rust-toolchain.toml`. Un `cargo`
nuevo puede encontrar un compilador o Clippy antiguo en `PATH`. En una consola
PowerShell podés seleccionar el toolchain de esta sesión así:

```powershell
$taskRustBin = Split-Path (rustup which --toolchain 1.98.0 cargo)
$env:PATH = "$taskRustBin;$env:PATH"
$env:RUSTC = Join-Path $taskRustBin 'rustc.exe'
$env:RUSTDOC = Join-Path $taskRustBin 'rustdoc.exe'
rustc -V
rustdoc -V
cargo clippy -V
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
node --test extension/tests/*.test.cjs
```

En Unix ejecutá también Clippy y tests con `--features daemon`. La matriz de
CI cubre Windows x86_64, Linux x86_64 y macOS Intel/Apple Silicon.

## Visor de archivos

El visor lee archivos regulares: no abre dispositivos, pipes ni sockets.
Muestra como máximo 2 MiB de bytes originales y 100.000 líneas, con un aviso
cuando recorta el contenido. Colorea las primeras 20.000 líneas de los archivos
sin líneas extensas; el resto permanece legible como texto plano. Una línea
extensa se muestra en continuaciones indicadas por `↳`, sin cortar el texto
retenido ni agregar saltos de línea a la copia. Los archivos con bytes NUL se identifican
como binarios. Abrir otro archivo o cerrar el visor descarta el trabajo anterior.

La lectura tiene un único worker. Una llamada al sistema detenida en una unidad
de red puede seguir esperando; cambiar de archivo no crea más hilos ni cancela
instantáneamente esa llamada. La app conserva el texto original para el parser,
incluidos CRLF y EOF, y pide repintado cuando termina de leer o colorear.
Los números de línea no forman parte del texto seleccionado al copiar código.

Si falla la lectura, el visor distingue una ruta ausente, acceso denegado,
un destino que no es un archivo regular y una lectura interrumpida. Muestra
la ruta completa y permite **Reintentar** después de resolver el problema.
El botón **Ruta** copia la ruta completa, también para nombres Unicode largos;
el nombre de la cabecera muestra esa ruta al pasar el mouse. No se publica un
documento parcial cuando falla una lectura después de recibir algunos bytes.
Un reintento reutiliza el lector activo; sólo inicia otro cuando el anterior
ya terminó o no pudo iniciarse. Reintentar no libera una llamada al sistema
que sigue bloqueada en un volumen remoto.

Podés seleccionar con arrastre o Shift+clic, y extender la selección con
Shift+flechas, Shift+Inicio y Shift+Fin. Ctrl+A (Cmd+A en macOS) selecciona todo el contenido
retenido, incluso fuera de pantalla. Copiar selección conserva los terminadores
originales; Copiar línea incluye la línea lógica completa aunque se muestre en
varias continuaciones. La barra y el menú contextual permiten copiar el archivo
completo. Los atajos pertenecen al visor sólo mientras tiene el foco; abrir la
paleta o volver a la terminal entrega el teclado a esa superficie.
Los botones del visor también retienen su entrada de teclado: Tab permite
alcanzarlos y Enter activarlos sin escribir esos eventos en el shell. Esc
cierra el visor durante un reintento; un diálogo abierto conserva su propio
Escape. Las acciones del visor quedan deshabilitadas al perder el foco de
ventana o mientras hay una superficie modal activa.
