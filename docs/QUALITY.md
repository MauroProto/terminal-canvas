# Revisión de calidad — septiembre de 2026

Base revisada: `88f67656700bac4cef83b094864d18d22d91e503`.
Los cambios se desarrollan en `codex/premium-quality` mediante commits pequeños
que permiten revisar y revertir cada corrección. Online, invitaciones y el
protocolo de colaboración quedan fuera de esta revisión por decisión del proyecto.

## Cambios que protegen el trabajo del usuario

| Área | Problema corregido | Resultado |
| --- | --- | --- |
| Layout e historial | Una instancia anterior podía escribir después de cambiar la propiedad de persistencia. | La escritura de layout/historial mantiene un bloqueo del sistema durante la operación y verifica la identidad de la instancia. Los fallos se muestran en la app. |
| Checkpoints | Grid, secuencia y log podían pertenecer a instantes distintos; el arranque frío tenía dos fuentes de restauración. | Snapshot y drenaje se coordinan bajo los mismos locks. El daemon restaura antes de iniciar el lector, y la UI evita repetir el historial. |
| Estado del terminal | Se perdían cursor, modos, marcas combinadas y estado de pantalla alternativa. | Checkpoints semánticos acotados, compatibles con el formato anterior, conservan cursor actual/guardado, wrap, modos, tabulaciones, márgenes y paleta. La compactación no corta secuencias ANSI. |
| Logs Windows | Un log truncado no podía repararse con un handle abierto sólo para append. | Se abre con permiso de escritura y se posiciona al final después de reparar. |
| Entrada y salida | Escrituras al PTY o socket podían detener la UI; frames grandes y la última ráfaga podían atascarse o perderse. | Colas acotadas, aceptación completa o rechazo visible, progreso de frames grandes y drenaje antes de notificar la salida del daemon. |
| Reconexión | Se descartaban bytes precargados por BufReader; un proceso terminado se confundía con transporte caído. | Se conserva el buffer, se distingue Exit y se cierran los enlaces al soltar el handle. Handshakes y mensajes de control tienen timeout. |
| ConPTY | El pipe de salida podía seguir abierto después de finalizar el shell. | Un observador del proceso informa su salida sin esperar EOF ni bloquear el frame. |
| Prompts pendientes | Una cola llena o un workspace inactivo podía dejar un prompt perdido o detenido. | Se conserva el prompt rechazado temporalmente y se reintenta también en workspaces ocultos. El tamaño se valida antes de retener una copia. |
| Preferencias y notas | Disco y configuración se procesaban en handlers de UI; el cierre podía perder el último cambio. | Un worker serializa operaciones, agrupa escrituras repetidas, entrega errores a la UI y drena lo pendiente al cerrar. |
| Worktrees | Limpiar podía eliminar archivos no confirmados o una carpeta usada por un terminal. | Archivo/restauración recuperables, verificación de uso activo y rechazo de eliminación con cambios, archivos sin seguimiento o ignorados. |
| Lanzamientos | Cancelar no siempre invalidaba un resultado ya preparado. | El resultado mantiene la identidad de la solicitud y se descarta al cancelar. El brief cruza el shell mediante una solicitud privada, con argumentos nativos para el agente. |

El daemon mencionado aquí es el proceso **local** de sesiones en Unix; no es
el servicio de Online/invitaciones.

## Interfaz, revisión y memoria

La paleta permite navegar todos sus resultados y mantiene visible la selección.
El visor dockeado y el terminal comparten el teclado según el foco real. La barra
de tareas permite desplazarse y muestra la terminal enfocada. Los atajos de
división usan Ctrl+Alt en Windows/Linux y evitan acciones duplicadas. Los controles
que no tienen efecto en el escritorio actual dejan de ofrecerse.

La revisión de Git conserva nombres con espacios, Unicode y comillas, archivos
nuevos, renombrados, binarios y cambios grandes. Diferencia un error de lectura
de un árbol limpio. Las notas conservan lado, identidad de revisión y archivo;
las notas antiguas siguen accesibles cuando el diff cambia. Buscar archivos
incluye configuración del proyecto sin recorrer los datos internos de `.git`.

La memoria usa identidad explícita de tarea, propagada desde el lanzamiento al
PTY, los hooks, la UI, la CLI y MCP. La migración de SQLite conserva los datos
existentes. Proponer una memoria no devuelve contenido pendiente de otra tarea.
La redacción conserva espacios y código; la deduplicación distingue cambios de
mayúsculas. Los presupuestos incluyen metadatos y serialización, y las consultas
tienen límites antes de cargar historiales grandes.

## Extensión y soporte

La extensión se activa desde el popup con `activeTab`, valida el tab y la ventana
al capturar y exige confirmación HTTP de la app. Un click generado por la página
no consume la selección. La captura elimina contenido ejecutable HTML/SVG y
valores de formularios, usa un documento inerte y limita profundidad, nodos,
texto e imagen. Los errores de red y las capturas sin imagen se informan.

El diagnóstico usa las mismas rutas que los escritores, limita el volumen de
lectura y redacta campos TOML/JSON estructurados. Los logs siguen requiriendo
revisión humana antes de compartirlos. Hay un formulario de errores y una
[guía de soporte y recuperación](SUPPORT.md).

La distribución distingue sistema y arquitectura, incluye los helpers de memoria,
genera checksums relativos al archivo descargado y prepara cuatro paquetes antes
de publicar una release. El comprobador selecciona únicamente el asset exacto.
Ver [instalación portable](PORTABLE.md) y [release](RELEASE.md).

## Verificación

La revisión de código cerró el 16 de septiembre de 2026 sobre
`b8e0c33c2cddd8af0cd4668e66ac11b6e4b47d78`. La corrida del
[PR #13, `35142399418`](https://github.com/MauroProto/terminal-canvas/actions/runs/35142399418)
terminó con sus siete jobs aprobados. La
[corrida del push, `35142392994`](https://github.com/MauroProto/terminal-canvas/actions/runs/35142392994)
también completó correctamente los seis jobs ejecutados; el benchmark sólo se
ejecuta en pull requests. Este registro agrega documentación a ese código
validado. Las verificaciones posteriores del HEAD y de la integración se pueden
consultar en el [PR #13](https://github.com/MauroProto/terminal-canvas/pull/13).

| Validación | Resultado en la matriz del PR |
| --- | --- |
| Ubuntu 24.04 x86_64 | Formato, Clippy y todos los targets aprobados, tanto base como con `daemon`. |
| Windows 2025 x86_64 | Clippy y todos los targets aprobados. El daemon Unix no se ejecuta en Windows. |
| macOS 15 aarch64 | Clippy y todos los targets aprobados, tanto base como con `daemon`. |
| macOS 15 Intel x86_64 | Clippy y todos los targets aprobados, tanto base como con `daemon`. |
| Extensión | Las nueve pruebas JavaScript aprobadas en un job independiente. |
| Auditoría | Aprobada después de actualizar `rustls` a 0.23.45 y regenerar `Cargo.lock` con Cargo. Persisten las advertencias de mantenimiento indicadas abajo. |
| Benchmark | Aprobado el umbral existente de degradación máxima del 15% frente a `origin/master`. |

La matriz utiliza Rust 1.98.0. Ejecuta `cargo fmt --all -- --check`,
`cargo clippy --all-targets --locked -- -D warnings` y
`cargo test --all-targets --locked --quiet --no-fail-fast`. En Unix también
comprueba y ejecuta todos los targets con `--features daemon`, incluyendo Clippy
con advertencias como errores. Clippy y tests se ejecutan secuencialmente en cada
runner. La extensión usa `node --test extension/tests/*.test.cjs`.

Las suites de runtime importan la librería real. La prueba con veinte PTY
conserva sus comprobaciones de salida, tamaño y finalización; la inspección del
grid redimensionado ahora usa su lock, sin interpretar contención de `try_lock`
como un fallo de resize.

El daemon toma la finalización del hilo lector como frontera antes del drenaje
final, en lugar de usar la señal de vida del proceso. Una regresión adicional
verifica que una señal de salida anticipada no descarte bytes posteriores.
La prueba original sigue exigiendo las 3000 líneas completas antes de `Exit` y
que no queden frames pendientes. Se separó la primera línea de los controles
que puede emitir Bash; no se redujo la aserción. Pasó diez ejecuciones dirigidas
consecutivas en Linux en la
[corrida `35137098426`](https://github.com/MauroProto/terminal-canvas/actions/runs/35137098426).

La restauración de TUI conserva la comprobación de entrada real a alternate
screen. Se corrigió `WireSpec::default()` para que use 80x24, igual que la
deserialización de campos omitidos, en vez de terminar abriendo un PTY de 1x1.

La prueba paralela mantiene ocho PTY, 512 KiB por sesión y el plazo original de
30 segundos. Conserva respuestas parciales y reintenta errores transitorios
contra una fecha límite monotónica fija. EOF, mensajes inválidos y errores
permanentes siguen provocando un fallo. En macOS se espera con `poll`, sin
cambiar opciones del socket después del cierre del peer. Las regresiones cubren
UTF-8 partido, datos precargados, EOF vacío o parcial y vencimiento del plazo.

El payload se verifica sin contar metadatos OSC; quitar un byte sigue siendo
 detectable por la regresión. Los dos marcadores de finalización los genera
Python después del payload y no aparecen completos en el comando enviado,
eliminando la dependencia del eco del shell. Se espera a todos los workers
antes de cerrar el daemon, incluso si alguno falla. La carga pasó cinco
repeticiones dirigidas en
[Linux, `35137938512`](https://github.com/MauroProto/terminal-canvas/actions/runs/35137938512),
y cinco con la condición final de marcadores en
[macOS Intel, `35142238258`](https://github.com/MauroProto/terminal-canvas/actions/runs/35142238258).
Estas corridas dirigidas corresponden a los ajustes progresivos; la matriz
completa citada al principio valida su combinación final.

Las verificaciones de esta continuación se ejecutaron en GitHub Actions, no en
la PC local. No se modificaron sus worktrees. No quedan scripts ni workflows
transitorios de reparación en el árbol final.

## Límites de esta entrega

- Windows restaura el contexto guardado, pero sus procesos ConPTY no sobreviven
  al cierre de la app. En Unix la continuidad del proceso requiere el daemon.
- Un checkpoint no reconstruye cualquier estado privado del parser o del TUI,
  ni revive un proceso después de reiniciar el equipo. Historial y colas son finitos.
- Ghostty sigue siendo experimental y queda fuera de la matriz de distribución.
- Los tests de interfaz usan egui; no sustituyen una prueba visual manual con
  monitores, escalas, drivers y sistemas de accesibilidad reales.
- Los tests de la extensión usan el runtime JavaScript con mocks; no certifican
  todas las páginas ni todas las versiones del navegador.
- La auditoría conserva cinco advertencias por dependencias sin mantenimiento,
  `bincode`, `paste`, `rustls-pemfile`, `ttf-parser` y `yaml-rust`. No se agregaron
  excepciones para ocultarlas. Cargo también informa incompatibilidad futura
  de `block` 0.1.6 en macOS.
- La prueba de sesiones reales de Claude sigue requiriendo datos de un entorno
  local y no forma parte de la ejecución automática. El fixture de subprocess
  marcado como ignorado se invoca explícitamente desde sus tests de regresión.
- No se ha publicado una release firmada, un instalador ni un tap Homebrew en
  esta revisión. El cask requiere los checksums de los DMG finales.
- La actualización se instala manualmente. Online e invitaciones conservan su
  implementación anterior y no reciben una nueva garantía de calidad por estos cambios.
