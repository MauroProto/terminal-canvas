# Estado de entrega y continuación — 7 de octubre de 2026

El último checkpoint publicado y validado es
`6d6c3f655d3e61f46646d1cf8c7814288d5a201c`, de **112 microcommits** desde
`f0458dd1dae08b3aca080a914042deb5a9454cf3`.
Su [CI candidata](https://github.com/MauroProto/terminal-canvas/actions/runs/37590871052),
[CI de master](https://github.com/MauroProto/terminal-canvas/actions/runs/37597403375)
y [ensayo de distribución](https://github.com/MauroProto/terminal-canvas/actions/runs/37591012832)
terminaron SUCCESS. El ensayo produjo cinco paquetes de prueba sin firma;
firma, notarización y publicación de producción quedaron omitidas.
El checkpoint amplía gramáticas y aliases del visor y registra la investigación
del serializador; no integra el fork experimental ni cambia Online/invitaciones.
Ver [HIGHLIGHT-SUPPORT.md](HIGHLIGHT-SUPPORT.md) y
[DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md).

El pase posterior de recuperación y exports se describe en
[PERSISTENCE-EXPORT-RECOVERY.md](PERSISTENCE-EXPORT-RECOVERY.md).
Necesita evidencia de CI y paquetes de su propio SHA antes de publicarse;
los resultados del checkpoint anterior no validan sus nuevas fuentes.

## Checkpoint gráfico anterior

El checkpoint gráfico de código validado fue `a93286e77054bbfd803c4503860ab488585103b0`, de
**97 microcommits** desde `f0458dd1dae08b3aca080a914042deb5a9454cf3`.
Su [CI completa](https://github.com/MauroProto/terminal-canvas/actions/runs/37576723374)
y su [ensayo de distribución](https://github.com/MauroProto/terminal-canvas/actions/runs/37576793676)
terminaron correctamente en Windows, Linux y macOS Intel/Apple Silicon.
Pasaron Clippy, las suites normales y de seguridad, las suites con daemon en
Unix, formato, contrato gráfico, extensión 9/9 y auditoría. El benchmark
comparativo no corre para pushes. Las suites se solapan y no deben sumarse
como casos únicos. Quedan dos avisos de mantenimiento, sin ignores nuevos.

El ensayo comprobó exactamente cinco paquetes y cinco checksums, helpers,
instalación/desinstalación Inno y montaje de ambos DMG. Firma, notarización y
publicación quedaron omitidas; sus paquetes son de prueba, sin firma del
proveedor. No se acredita una release de producción publicada.

El cierre gráfico actualizó GRAPHICS-MIGRATION y este documento después del código
validado, además de una fixture TLS incluida sólo bajo `#[cfg(test)]`. Los
documentos no se copian dentro de los paquetes y la fixture no se compila
para sus binarios de producción. El recibo
externo de continuación debe registrar el HEAD documental posterior, su CI
exacta, mapas, bundles y SHA256 al cerrar la entrega. Las corridas anteriores
sólo acreditan sus propios
commits; comparar las fuentes no demuestra identidad binaria de recompilaciones.

## Implementación actual

El stack usa egui/eframe/egui_kittest 0.36.2 y wgpu 30.0.1. El mínimo de Rust
es 1.95; la matriz usa 1.98.0. `App::logic` procesa PTY, workers, restauración,
ACK, autosave y pedidos de repaint aun sin un pass de dibujo. `App::ui`
procesa teclado, puntero, animaciones y paneles. La entrada retenida por egui
al ocultar la ventana no se reenvía desde lógica; el foco se toma de la entrada
raw actual. Cada fase conserva su propio contador de panics.

Sidebar, barra de tareas, visor y canvas comparten la UI raíz. Los overlays
y comandos reciben el rectángulo restante antes de que el panel central lo
consuma. El visor conserva su borde personalizado y su ancho completo.
El render ancla clusters Unicode y caracteres combinados a sus columnas;
agrupa ASCII ordinario y usa anclaje por celda cuando ligaduras, kerning o
avances no uniformes podrían desplazarlo. La caché considera revisión,
tamaño real de fuente, DPI y generación del atlas. El fallback de negrita
conserva la fuente primaria y los símbolos/emoji.

Se conservan accesibilidad, clipboard, enlaces, X11, Wayland y los backends
nativos. Las decoraciones Wayland usan crossfont con Fontconfig/FreeType;
ver [PORTABLE.md](PORTABLE.md). El lockfile ya no incluye `paste`,
`ttf-parser`, `ab_glyph` ni el backend antiguo `metal`. El verificador
`scripts/graphics-deps-verify.py` comprueba features de ventanas, parsers
retirados y compatibilidad de bindings Direct3D. Ver los detalles y la matriz
nativa en [GRAPHICS-MIGRATION.md](GRAPHICS-MIGRATION.md).

Los arreglos previos de persistencia siguen implementados: ACK asociados a la
sesión runtime capturada, barreras antes del cierre, captura inicial y
escrituras en FIFO, rescate de salida nueva durante replay, alias legacy,
rollback de append y lectura estricta. Un snapshot o manager no disponible
no elimina una hoja del conjunto que debe guardarse. Un checkpoint nuevo
excluye generaciones de logs retenidos, incluido el wraparound. Fallos de
layout, configuración o historial mantienen el marcador de recuperación;
la poda espera el layout confirmado. Si falla la captura inicial, la app
conserva los archivos anteriores, pausa su persistencia durante esa ejecución
y avisa que la salida nueva queda en memoria y debe copiarse antes de reiniciar.
Cerrar el panel no elimina esa guarda.

También están implementados el lanzador Windows contenido en un Job, el visor
con lector/resaltador acotados y cancelables, la conservación de saltos de
línea y la invalidación del render al cambiar la fuente. La cancelación del
visor no interrumpe una syscall ni una regex ya bloqueadas. Revisar las
regresiones existentes antes de repetir estos arreglos.
Online e invitaciones quedan fuera de cambios funcionales.

## Evidencia actual y checkpoints históricos

El componente gráfico aislado aprobó **40 pruebas**, incluidas cuatro de
fuentes, y Clippy. Comprueba medidas, columnas, render completo/reducido y
caché, con pares ASCII en 54 combinaciones de fuente, tamaño, zoom y DPI.
La regresión de columnas Unicode falló antes del arreglo; también hay
regresiones de ligaduras/kerning con fuentes embebidas. Ese harness no
ejecuta FFI Ghostty, GPU, PTY ni una ventana real. Los casos de fuentes
forman parte de las 40 pruebas; no se suman otra vez.

Las nuevas fixtures de entrada retenida, contadores y bounds usan paneles
detached, sin shell ni PTY real; la regresión de persistencia oculta usa un
PTY en memoria y un perfil aislado. La integración de estos casos pasó
la CI del checkpoint de código indicado arriba. No se ejecutaron builds
completos ni una GUI de la app en esta PC durante la migración gráfica.

La [CI anterior, 37574525612](https://github.com/MauroProto/terminal-canvas/actions/runs/37574525612),
del candidato `6a7e78e0d9b6b667ac6cd8a518d9eaac4a92285f`, terminó FAILURE:
Windows e Intel detectaron sólo una aserción de la fixture de repaint oculto.
Un Restore pendiente pedía 16 ms antes del fallback de 2 s; egui conserva el
menor plazo y no emite un callback por cada pedido. El nuevo test separa el
scheduler de los pollers, comprueba primero un tick vacío sin callbacks y luego
exige un plazo positivo y acotado en cada tick. Conserva las aserciones de
entrada retenida, foco, unread, ausencia de passes UI y de PTY real; retirar
el fallback de producción deja el nuevo test sin callback y lo hace fallar.
El cambio afecta sólo pruebas, no la lógica de producción.

La [CI del cierre documental previo, 37580301965](https://github.com/MauroProto/terminal-canvas/actions/runs/37580301965),
de `efa33bb44bd10a8fca9903d24c69dd175368d8b3`, detectó otra carrera en una
fixture TLS de Linux: la suite normal pasó 979 casos y el filtro de seguridad
19, pero el cleanup de un socket ya desconectado falló después de que el
cliente comprobara HTTP 307 y cuerpo vacío. La misma fixture había pasado
normal/seguridad/daemon del checkpoint de código. Se acepta únicamente
`NotConnected` al hacer shutdown después de completar request, respuesta y
flush estrictos. Otros errores siguen fallando y los asserts de redirect,
certificado, status y cuerpo se conservan. Sólo cambia el módulo de tests,
sin cambios funcionales de Online ni invitaciones. El recibo externo debe
acreditar por separado la CI exacta del HEAD corregido.

El [ensayo anterior, 37575466330](https://github.com/MauroProto/terminal-canvas/actions/runs/37575466330),
del candidato `6a7e78e0d9b6b667ac6cd8a518d9eaac4a92285f`, terminó CANCELLED
tras confirmar la fixture fallida de repaint oculto en el preflight daemon
de macOS ARM. Esta corrida precede al fallo de cleanup TLS descrito arriba.
Dejó **cero artefactos** y no validó paquetes,
instaladores, DMG, checksums ni publicación. Esas corridas son evidencia del
fallo corregido, no una aprobación del nuevo candidato.

La [auditoría del checkpoint gráfico](https://github.com/MauroProto/terminal-canvas/actions/runs/37576723374)
pasó sin vulnerabilidades reportadas y con **dos avisos de mantenimiento**,
`bincode 1.3.3` y `rustls-pemfile 2.2.0`, sin ignores nuevos. El resultado
acredita esa consulta y no es una garantía permanente. Ver [DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md).

| Checkpoint histórico validado | Código/HEAD documental | Evidencia cerrada |
| --- | --- | --- |
| 70 microcommits | Código `62bc6e73008e9d57f9011e36b4ef6ce4c77a902d`; HEAD documental `c2b4aca415bba4dbf591cd4aa71c6118d824b580` | [CI código](https://github.com/MauroProto/terminal-canvas/actions/runs/37567581842), [CI documental](https://github.com/MauroProto/terminal-canvas/actions/runs/37567972413) y [paquetes del código](https://github.com/MauroProto/terminal-canvas/actions/runs/37567632160): SUCCESS. |
| 60 microcommits | `0dd00e83e10dbc484a7b1a6c9c2d9f6a642e8d96` | [CI](https://github.com/MauroProto/terminal-canvas/actions/runs/37565219411) y [paquetes](https://github.com/MauroProto/terminal-canvas/actions/runs/37565273400): SUCCESS. |
| 45 microcommits | `65d81a7`, contención Windows | [CI](https://github.com/MauroProto/terminal-canvas/actions/runs/37558769570) y [paquetes](https://github.com/MauroProto/terminal-canvas/actions/runs/37558823335): SUCCESS. |

Los checkpoints históricos pasaron las matrices de Windows, Linux y ambos
macOS; en Unix incluyeron daemon. Las suites normal, seguridad y daemon se
registran por separado y se solapan: no sumar sus recuentos como casos únicos.
Los ensayos comprobaron exactamente cinco paquetes y cinco checksums,
helpers, instalación/desinstalación Inno y montaje de ambos DMG. Firma,
notarización y publicación quedaron omitidas. El benchmark comparativo sólo
corre para pull requests. Los cuatro avisos de mantenimiento de esos
lockfiles anteriores son evidencia histórica; el candidato gráfico tiene
los dos avisos descritos arriba.

Para cerrar el candidato actual, exigir éxito de los cuatro jobs de plataforma,
Clippy, suites normales/seguridad/daemon, extensión y auditoría, además del
ensayo completo con `verify-set`. Registrar cada SHA y recibo por separado.
Si el HEAD final sólo añade documentación no empaquetada y fixtures bajo
`#[cfg(test)]`, distinguirlo del
SHA de código validado y del SHA de los paquetes. `LICENSE` y
`docs/PORTABLE.md` se copian dentro de los paquetes: verificarlos con el mismo
checkout del ensayo. Comparar inputs no demuestra que una recompilación
produzca binarios idénticos. Una publicación real reconstruye desde su tag.

## Pendientes concretos

1. Validar en hardware nativo, con perfil aislado y copia de datos: GPU/driver,
   fuentes ASCII/CJK/emoji/combining/negrita, ligaduras, DPI/zoom, clipboard,
   accesibilidad, paneles/visor/paleta, resize, minimizar/ocultar, salida continua,
   autosave, cierre y terminación abrupta. Comparar layout e historial al reabrir.
   Medir fluidez, latencia, CPU/GPU y memoria con varias terminales activas.
   Probar ConPTY en Windows, daemon/reconexión en Unix y agentes reales.
   En Wayland comprobar decoraciones del cliente y del compositor. Ghostty
   sigue experimental fuera de la matriz de distribución.
2. Configurar Authenticode con timestamp y Developer ID/notarización con
   credenciales reales; probar upgrade desde una versión firmada por el mismo
   editor, cancelación de descarga, error de disco, publisher distinto y
   sesiones vivas. Los helpers de instalación tienen timeout, límite de salida
   y cleanup; no ofrecen cancelación mediante un token del usuario.
3. Publicar después de validar candidato, dry-run y firmas. Comprobar los
   paquetes descargados de la release real. Ver [RELEASE.md](RELEASE.md).
4. Actualizar versión/SHA256 del cask con los DMG finales y publicar Homebrew
   con acceso real. Revisar protección de rama y permisos en instalaciones reales.
5. Resolver los dos avisos sin ignores: validar la integración del serializador
   con las APIs, los assets y los límites de memoria de syntect/two-face. El
   prototipo aislado conservó compatibilidad con los assets antiguos; no prueba
   que sea obligatorio regenerarlos. PEM afecta TLS/colaboración y requiere
   un alcance compatible con la exclusión de Online/invitaciones.
6. Acotar el visor cuando una sola línea ocupa gran parte de los 2 MiB: el
   límite por archivo y la virtualización de filas no limitan el galley ni la
   regex de esa línea. Segmentar en el worker y conservar offsets originales
   para copiar selecciones sin añadir saltos visuales. No ocultar ni truncar
   silenciosamente la fuente para solucionar el coste de render.

## Prompt para continuar en otra máquina

> Continuá Terminal Canvas (`MauroProto/terminal-canvas`) desde el HEAD publicado
> de `master`; conservá los checkpoints anteriores. El último checkpoint cerrado
> es `6d6c3f655d3e61f46646d1cf8c7814288d5a201c`, de 112 microcommits, con CI
> 37590871052/37597403375 y Release 37591012832 aprobadas. El pase posterior de
> persistencia/exports necesita sus propios recibos con SHA exacta.
> Comprobá HEAD, rama, estado,
> remoto y SHAs de las corridas antes de editar; no atribuyas éxito a otro SHA.
> Leé este documento, PERSISTENCE-EXPORT-RECOVERY, HIGHLIGHT-SUPPORT,
> GRAPHICS-MIGRATION, DEPENDENCY-MAINTENANCE, RELEASE,
> PORTABLE y SUPPORT. Conservá todos los microcommits y el revert bc5768d;
> integrá por fast-forward en master, sin force push ni descartar datos.
> Los IDs originales difieren de los publicados por Git Data API: no mezcles
> ambos historiales. Ya están implementadas egui/eframe/kittest 0.36.2 y
> wgpu 30.0.1, separación logic/ui, polling y persistencia con ventana oculta,
> contadores por fase, UI raíz, bounds, fallback de fuentes y columnas
> ASCII/Unicode, además de las guardas de persistencia descritas arriba.
> Revisá sus regresiones; no rehagas esos arreglos sin un fallo reproducible.
> Online e invitaciones quedan fuera de cambios funcionales.
> En otra máquina adecuada, probá GPU/driver, UI, PTY y agentes reales con
> perfiles aislados y copia de datos, incluyendo minimizar/ocultar, autosave,
> cierre y recuperación. Medí fluidez, latencia y consumo con varias terminales
> activas: los benchmarks actuales no miden el dibujo real en GPU.
> Después cerrá firmas/notarización, upgrade firmado,
> publicación y Homebrew con acceso real, y el mantenimiento permitido de
> bincode/rustls-pemfile. No inventes credenciales ni resultados.
> La PC de Mauro se sobrecargó: allí no ejecutar builds completos, GUI, WSL ni
> servidores; usar GitHub para integración y paquetes. Componentes pequeños:
> Rust 1.98.0 con PATH/RUSTC/RUSTDOC coherentes, un job, hasta dos threads,
> prioridad Idle y procesos ocultos. Corregí fallos en microcommits y reportá
> SHA, evidencia y límites por separado. Las pruebas headless no acreditan
> apariencia nativa, consumo de GPU, accesibilidad real ni entrega firmada.

## Publicación y respaldo

La rama principal se llama `master`; la reversión pedida quedó integrada en
`bc5768d`. Git Data API generó IDs y metadatos diferentes de los commits
locales originales. El CLI de esta PC conserva su cuenta de sólo lectura;
la publicación usa el conector autorizado. Los mapas comprueban árboles,
padres y mensajes antes de avanzar la rama. Usar la historia publicada para
continuar; los originales son respaldo, no una rama para mezclar.

El checkpoint de 70 commits conserva sus bundles y recibos externos. El cierre
gráfico debe adjuntar un manifiesto con los SHAs público/original, código
validado, paquetes y HEAD documental, las corridas exactas y hashes de cada
archivo. Sus bundles previstos son
`terminal-canvas-graphics-final-published-20261007.bundle` (base `f0458dd1`) y
`terminal-canvas-graphics-final-original-v2-20261007.bundle` (base `c2b4aca`);
la creación y verificación de los bundles se registra por separado en el recibo
externo, después de comprobar sus refs y hashes. Los bundles no
contienen `dist/` ni datos del perfil; los recibos se entregan aparte.

Preferir un clon actualizado de master. Con bundles, comprobar la ref y los
prerrequisitos mediante `git bundle verify`, usando la base completa indicada
en el manifiesto. Para actualizar un checkout limpio:

```bash
git fetch origin
git status --short
git switch master
git merge --ff-only origin/master
```

Si hay cambios locales o el fast-forward falla, inspeccionarlos sin descartar
datos. Verificar SHA256SUMS y recibos del paquete de continuación. No incluir
tokens, certificados, contraseñas ni contenido privado de perfiles en una
conversación o en evidencia de pruebas.
