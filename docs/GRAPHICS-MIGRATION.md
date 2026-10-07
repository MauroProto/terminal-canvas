# Migración del stack gráfico

El candidato del 7 de octubre de 2026 usa egui/eframe/egui_kittest 0.36.2 y
wgpu 30.0.1. El lockfile ya no contiene `paste`, `ttf-parser`, `ab_glyph` ni
el backend antiguo `metal`. El mínimo de Rust es 1.95; la matriz usa 1.98.0.
Las API nuevas se contrastaron con las
[fuentes de eframe 0.36.2](https://github.com/emilk/egui/tree/0.36.2/crates/eframe)
y las [features de winit 0.30.13](https://github.com/rust-windowing/winit/blob/v0.30.13/Cargo.toml).

## Comportamiento conservado

`App::logic` procesa PTY, restauración, workers, ACK de persistencia y autosave
incluso sin un pass de dibujo. `App::ui` procesa teclado, puntero, animaciones,
paneles y overlays. La entrada retenida por egui mientras la ventana está
oculta no se vuelve a enviar al terminal. Cada fase conserva su propio contador
de panics; un éxito de lógica no borra una racha de fallos de dibujo.
Las barreras de cierre, la propiedad de escritura y el marcador de recuperación
conservan su comportamiento anterior.

Sidebar, barra de tareas, visor y canvas comparten la UI raíz. Los overlays y
comandos reciben el rectángulo restante antes de que el panel central lo consuma.
El visor conserva su borde personalizado y su ancho completo.

Los clusters Unicode y sus caracteres combinados se dibujan en su columna.
Los tramos ASCII ordinarios se mantienen agrupados. La caché sigue considerando
revisión, tamaño real de fuente, DPI y generación del atlas. El fallback de
negrita conserva la fuente primaria y las fuentes de símbolos/emoji.
El spacing ASCII se ajusta al avance real de la fuente. Si aparecen ligaduras,
kerning o avances no uniformes, cada celda se ancla por separado; los clusters
Unicode no reciben ese spacing adicional.

Se conservan accesibilidad, clipboard, apertura de enlaces, X11, Wayland y los
backends de render anteriores. Las decoraciones Wayland usan crossfont con
Fontconfig/FreeType; los requisitos están en [PORTABLE.md](PORTABLE.md).
El lockfile alinea los tipos Direct3D de wgpu-hal y gpu-allocator. El script
`scripts/graphics-deps-verify.py` detecta la reintroducción de parsers retirados,
features de ventanas perdidas y bindings Direct3D incompatibles.
Online e invitaciones no reciben cambios funcionales.

## Evidencia del candidato

El código validado es `a93286e77054bbfd803c4503860ab488585103b0`, de 97 microcommits desde
la base revisada. Su [CI completa](https://github.com/MauroProto/terminal-canvas/actions/runs/37576723374)
y su [ensayo de paquetes](https://github.com/MauroProto/terminal-canvas/actions/runs/37576793676)
terminaron correctamente. Los cuatro jobs de plataforma pasaron Clippy,
suites normales y seguridad; Unix también check/Clippy/tests con daemon.
Formato, contrato gráfico, extensión 9/9 y auditoría pasaron. El benchmark
comparativo se omite en pushes; los smoke tests de benchmarks sí corrieron.
El ensayo verificó exactamente cinco paquetes y cinco checksums, helpers,
instalación/desinstalación Inno y ambos DMG montados. Firma, notarización y
publicación se omitieron. El cierre documental posterior y su CI exacta
deben registrarse en el recibo externo; los paquetes corresponden al código indicado.

El componente gráfico aislado aprobó **40 pruebas**, incluidas cuatro de
fuentes, y Clippy: medidas, columnas, render completo/reducido y caché, con
pares ASCII en 54 combinaciones de fuente, tamaño, zoom y DPI. La regresión
Unicode falló antes del arreglo; las regresiones de ligaduras/kerning usan
Ubuntu-Light embebida. Ese harness usa tipos de snapshot copiados para
Ghostty; no ejecuta FFI, GPU, PTY ni una ventana real. No sumar los cuatro
casos de fuentes otra vez ni las suites normales/daemon como casos únicos.

| CI histórica | SHA propio | Resultado y corrección posterior |
| --- | --- | --- |
| [1: 37571808366](https://github.com/MauroProto/terminal-canvas/actions/runs/37571808366) | `73ba297` | FAILURE: métodos de DroppedFile, índices ByteIndex y bindings Direct3D incompatibles; corregidos. |
| [2: 37572505071](https://github.com/MauroProto/terminal-canvas/actions/runs/37572505071) | `96ff357` | FAILURE: import ByteRangeExt de una fixture; corregido, producción intacta. |
| [3: 37573217610](https://github.com/MauroProto/terminal-canvas/actions/runs/37573217610) | `c7745cb` | Detectó cleanup de TexturesDelta y supuesto de panel sin runtime; terminó CANCELLED tras un push posterior. Fixtures corregidas sin reducir bounds ni aserciones. |
| [4: 37574525612](https://github.com/MauroProto/terminal-canvas/actions/runs/37574525612) | `6a7e78e` | FAILURE: sólo fixture de repaint oculto en Windows/Intel; se separó el test de fallback del polling asíncrono. |

La fixture anterior pedía ver un callback cercano a 2 s aunque Restore
pudiera pedir 16 ms antes; egui sólo notifica un mínimo nuevo. El test nuevo
usa un scheduler aislado, un tick vacío como control y exige un deadline
positivo y acotado por tick. Retirar el fallback de producción lo hace fallar.
Las aserciones de entrada retenida, foco, unread, passes UI y ausencia de PTY
real siguen intactas. El cambio afecta sólo `src/app/tests.rs`.

El [ensayo anterior, Release37575466330](https://github.com/MauroProto/terminal-canvas/actions/runs/37575466330),
de `6a7e78e`, terminó CANCELLED después de confirmar la misma fixture en
daemon macOS ARM. Dejó **cero artefactos**: no acreditó paquetes, checksums,
helpers, instaladores, DMG ni publicación. Las corridas anteriores sólo
acreditan sus propios resultados y nunca los arreglos posteriores.

La auditoría de `a93286e` pasó sin vulnerabilidades reportadas y con dos
avisos de mantenimiento, `bincode 1.3.3` y `rustls-pemfile 2.2.0`, sin ignores
nuevos. Es evidencia de esa consulta, no una garantía permanente; ver
[DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md).

La validación nativa de GPU/driver, fuentes, clipboard, accesibilidad, PTY y
agentes reales sigue pendiente, además de firma/notarización, upgrades
firmados, publicación y Homebrew. Las pruebas headless y los paquetes de
ensayo sin firma no acreditan esas tareas. Online e invitaciones permanecen
excluidos de cambios funcionales.

## Validación nativa pendiente

En una máquina adecuada, usar un perfil aislado según [SUPPORT.md](SUPPORT.md).
No ejecutar estas pruebas gráficas en la PC de Mauro durante este pase.

| Entorno | Comprobaciones necesarias | Estado |
| --- | --- | --- |
| Windows x86_64 | Direct3D, ConPTY, DPI 100/125/150/200 %, clipboard, resize y minimizar/restaurar. | Pendiente |
| macOS Intel y Apple Silicon | Metal, Retina y pantalla externa, clipboard, resize, ocultar y reconexión al daemon. | Pendiente |
| Linux X11 | Render, clipboard, resize, fuentes y reconexión al daemon. | Pendiente |
| Linux Wayland | Lo anterior, más título y controles con decoraciones del cliente y del compositor. | Pendiente |

En cada entorno, contrastar cursor y columnas con ASCII (`!=`, `->`, `==`,
`fi`, `AV`), negrita, CJK, emoji y caracteres combinados, a varios tamaños/zooms.
Repetir con la fuente habitual del sistema y una fuente con ligaduras.
Comprobar sidebar, barra y visor juntos, abrir paleta/quick-open y comparar sus
bounds. Mantener salida activa mientras la ventana está oculta, reabrirla,
cerrar normalmente y repetir una terminación abrupta; comparar layout e
historial con la copia inicial. Registrar sistema, driver, escala, fuente,
commit y resultado sin incluir contenido privado.

Medir fluidez, latencia y consumo de CPU/GPU/memoria con varias terminales
activas y al cambiar fuente o DPI. Los benchmarks actuales comprueban
serialización de scrollback, diff y highlighting; no miden el dibujo del grid
en GPU ni la latencia de la ventana. Registrar una comparación en el mismo
hardware antes de acreditar rendimiento nativo.

Las pruebas headless y los paquetes sin firma no acreditan apariencia nativa,
accesibilidad real, consumo de GPU ni upgrades firmados. Ghostty conserva su
estado experimental fuera de la matriz de distribución.
