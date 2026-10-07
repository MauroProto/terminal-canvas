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

## Diagnósticos

La exportación de diagnóstico recoge configuración, layout y logs con límites
de tamaño. Redacta campos estructurados sensibles y omite una configuración
que no se pueda interpretar con seguridad. Los mensajes de error y logs aún
pueden contener texto privado: revisá el ZIP antes de adjuntarlo a un issue.
No hace falta publicar la base de memoria ni el historial completo para
reportar un problema de interfaz.

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
rustc -V
cargo clippy -V
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
node --test extension/tests/*.test.cjs
```

En Unix ejecutá también Clippy y tests con `--features daemon`. La matriz de
CI cubre Windows x86_64, Linux x86_64 y macOS Intel/Apple Silicon.
