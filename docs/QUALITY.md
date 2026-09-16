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

La matriz usa Rust 1.98.0 y compila Windows x86_64, Linux x86_64 y macOS
x86_64/aarch64. Incluye formato, Clippy con advertencias como errores, tests de
todos los targets, tests del daemon en Unix, pruebas de la extensión y auditoría
de dependencias. Las suites de runtime importan la librería real, evitando copias
del código bajo prueba. Hay una prueba con veinte PTY reales, además de pruebas
de interacción egui y regresiones de almacenamiento y reconexión.

En Windows pasaron `cargo fmt`, Clippy con advertencias como errores y
`cargo test --all-targets --locked --quiet --no-fail-fast`, además de las nueve
pruebas de la extensión. La corrida remota
[`34648292200`](https://github.com/MauroProto/terminal-canvas/actions/runs/34648292200)
sobre `c766dff` dejó verdes la auditoría, Windows y las suites base de ambos
macOS. Antes de integrar a `master` quedan por resolver tres regresiones Unix:
el orden entre la última ráfaga y `Exit` en Ubuntu, la detección de entrada a la
pantalla alternativa en ambos macOS y reintentos ante `WouldBlock` en la prueba
paralela de PTY de macOS Intel. El ajuste de detección de pantalla alternativa
quedó preparado en el commit inmediatamente posterior, pendiente de validación
en la matriz.

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
- No se ha publicado una release firmada, un instalador ni un tap Homebrew en
  esta revisión. El cask requiere los checksums de los DMG finales.
- La actualización se instala manualmente. Online e invitaciones conservan su
  implementación anterior y no reciben una nueva garantía de calidad por estos cambios.
