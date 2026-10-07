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

Se conservan accesibilidad, clipboard, apertura de enlaces, X11, Wayland y los
backends de render anteriores. Las decoraciones Wayland usan crossfont con
Fontconfig/FreeType; los requisitos están en [PORTABLE.md](PORTABLE.md).
El lockfile alinea los tipos Direct3D de wgpu-hal y gpu-allocator. El script
`scripts/graphics-deps-verify.py` detecta la reintroducción de parsers retirados,
features de ventanas perdidas y bindings Direct3D incompatibles.
Online e invitaciones no reciben cambios funcionales.

## Evidencia del candidato

La primera [CI, 37571808366](https://github.com/MauroProto/terminal-canvas/actions/runs/37571808366),
detectó rutas de archivos arrastrados, índices de texto tipados y dos versiones
incompatibles de los bindings Direct3D. Esos fallos se corrigieron en microcommits.
Su auditoría terminó con sólo dos avisos de mantenimiento, `bincode 1.3.3` y
`rustls-pemfile 2.2.0`, sin vulnerabilidades reportadas ni ignores nuevos.
Los avisos restantes se describen en [DEPENDENCY-MAINTENANCE.md](DEPENDENCY-MAINTENANCE.md).

El componente gráfico aislado aprobó 36 pruebas: fuentes, medidas, columnas,
render completo/reducido y caché. Incluye un probe de pares ASCII en 54
combinaciones de fuente, tamaño, zoom y DPI. Los caminos Ghostty de ese harness
usan tipos de snapshot copiados; no ejecutan FFI, PTY ni una ventana real.
Los cuatro tests de fuentes forman parte de esos casos, no son cuatro casos
adicionales. La regresión de columnas Unicode falló antes del arreglo.

La [segunda CI, 37572505071](https://github.com/MauroProto/terminal-canvas/actions/runs/37572505071),
corresponde al candidato `96ff357e746f82e352bd906effb2a1efc4443e95`.
Los resultados de una corrida sólo acreditan su propio commit. La aprobación
de la matriz y del ensayo de paquetes debe verificarse en el candidato final
antes de promoverlo a la rama principal.

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

Las pruebas headless y los paquetes sin firma no acreditan apariencia nativa,
accesibilidad real, consumo de GPU ni upgrades firmados. Ghostty conserva su
estado experimental fuera de la matriz de distribución.
