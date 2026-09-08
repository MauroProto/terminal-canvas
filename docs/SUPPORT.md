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

Para hacer una copia antes de investigar un problema de persistencia, cerrá
las instancias de la app y guardá una copia completa de sus directorios de
datos y configuración. Si copiás SQLite, incluí también sus archivos `-wal`
y `-shm` cuando existan. Conservá la copia original para comparar resultados.

Si aparece un aviso de que otra instancia tiene la persistencia, cerrá la
instancia anterior y reabrí la que vas a usar. La propiedad de escritura evita
que una ventana anterior sobrescriba el estado más reciente.

## Diagnósticos

La exportación de diagnóstico recoge configuración, layout y logs con límites
de tamaño. Redacta campos estructurados sensibles y omite una configuración
que no se pueda interpretar con seguridad. Los mensajes de error y logs aún
pueden contener texto privado: revisá el ZIP antes de adjuntarlo a un issue.
No hace falta publicar la base de memoria ni el historial completo para
reportar un problema de interfaz.

## Instalación y actualización

Consultá [PORTABLE.md](PORTABLE.md) para elegir arquitectura, verificar el
checksum y conservar los helpers junto a la app. La actualización se instala
manualmente; el comprobador abre la descarga correspondiente. Los scripts y
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
