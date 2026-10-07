# Dependencias con avisos de mantenimiento

La migración gráfica del 7 de octubre de 2026 usa egui/eframe/egui_kittest
0.36.2 y wgpu 30.0.1. La [CI de referencia del 7 de octubre](https://github.com/MauroProto/terminal-canvas/actions/runs/37583514833)
`c23317eaa2007ebcde61c12d243a379c42511b44` pasó; su auditoría no reportó
vulnerabilidades y mantuvo dos avisos `INFO Unmaintained`: `bincode 1.3.3`
([RUSTSEC-2025-0141](https://rustsec.org/advisories/RUSTSEC-2025-0141.html))
y `rustls-pemfile 2.2.0`
([RUSTSEC-2025-0134](https://rustsec.org/advisories/RUSTSEC-2025-0134.html)).
No se agregaron ignores. El resultado corresponde a esa consulta y ese
lockfile, no es una garantía permanente.

`Cargo.lock` ya no contiene `paste`, `ttf-parser`, `ab_glyph`,
`owned_ttf_parser`, el backend antiguo `metal`, `yaml-rust` ni `block`.
La integración y las comprobaciones nativas se registran por separado en
[GRAPHICS-MIGRATION.md](GRAPHICS-MIGRATION.md).
La auditoría local de `dist/validation-audit.json`, del 3 de octubre,
y la [auditoría de GitHub del 6 de octubre](https://github.com/MauroProto/terminal-canvas/actions/runs/37544616708/job/112545511009)
son evidencia histórica del lockfile anterior, que tenía cuatro avisos.
La [auditoría inicial de la migración gráfica](https://github.com/MauroProto/terminal-canvas/actions/runs/37573217610)
`c7745cb2251c9d6c761842a501883d0f695601a1` también registró los dos
avisos actuales, antes de la integración y las correcciones posteriores.

## Rutas que deben migrarse

| Dependencia en el lockfile | Cómo llega a la app | Trabajo necesario |
| --- | --- | --- |
| `bincode 1.3.3` | `syntect 5.3.0`, usado directamente y por `two-face 0.5.2` | El highlighting carga assets embebidos serializados. Un prototipo aislado comprobó compatibilidad bidireccional con los assets antiguos sin exigir regeneración. Su integración sigue pendiente por API, memoria y mantenimiento; requiere validar también la app, rendimiento y distribución. No es el formato de layout o historial propio de la app. |
| `rustls-pemfile 2.2.0` | Dependencia directa; también `axum-server` y `rustls-native-certs` a través de WebSocket/TLS | Hay releases que eliminan las rutas transitivas, además del reemplazo directo por `rustls-pki-types::pem::PemObject`. La migración cambia APIs, aceptación de raíces y buffers de WebSocket; afecta TLS/colaboración y está fuera del pase que excluye Online e invitaciones. |

## Evidencia y criterios de cierre

### Prototipo de serializador: comprobado, sin integrar

La [release publicada de syntect](https://github.com/trishume/syntect/releases/tag/v5.3.0),
consultada el 7 de octubre, sigue siendo 5.3.0 y su
[manifest](https://github.com/trishume/syntect/blob/v5.3.0/Cargo.toml)
activa bincode mediante parsing y carga/creación de dumps. El
[PR oficial #694](https://github.com/trishume/syntect/pull/694), head
`509deb8944df323d9a70915edf0bbd618e5d8240`, propone `serde-wincode`;
sigue abierto y sin reviews, y no constituye una release nueva.

Se preparó bajo `dist/` un backport acotado sobre syntect 5.3.0 con
[`serde-wincode 0.1.2`](https://github.com/A-Manning/serde-wincode/tree/v0.1.2),
[`wincode 0.6.2`](https://github.com/anza-xyz/wincode/releases/tag/wincode%40v0.6.2)
y un `DumpError` mínimo basado en la parte de dumps del
[PR #625](https://github.com/trishume/syntect/pull/625). No copia los demás
cambios de parser/gramáticas de la rama del PR. **El prototipo no está
integrado en los manifests ni el lockfile de la app y no elimina su aviso.**

El componente aislado en Windows x86_64 comparó el módulo de highlighting
congelado de `c23317e` con el serializador anterior y el prototipo:
213 gramáticas con saltos de línea y 213 sin ellos, sus contextos lazy,
32 temas, 59 casos de corpus y siete dumps conservados. El prototipo leyó
los siete dumps antiguos; el exporter antiguo conservado leyó los siete
generados por el prototipo. Coincidieron los valores canónicos de catálogo,
temas y salida por carácter. Esta evidencia muestra que el cambio no exige
regenerar esos assets; no afirma identidad binaria de dumps, porque el
orden de HashMap y la compresión pueden cambiar los bytes. El recibo local
`dist/validation-highlight-serializer-investigation.json` registra fuentes,
locks, hashes y resultados de las tres ejecuciones.

Las tres muestras de tiempo por ejecución usaron prioridad Idle, afinidad
a dos CPU y límite de CPU del 20 %. Conservan efectos de caches del OS,
allocator e internado de scopes; no establecen una garantía de rendimiento.
La memoria máxima del Job incluye los procesos hijos y, cuando existen,
Cargo/compilador/linker: no mide la memoria de la app. No se probaron GUI,
GPU, PTY, agentes reales ni comportamiento nativo de la app. Tampoco se
acreditan con esas ejecuciones los cambios de selector posteriores a `c23317e`
ni equivalencia exhaustiva del parser para todas las entradas.

Se posterga la adopción del fork por diferencias y obligaciones concretas:

- Los resultados públicos dejan de usar `bincode::Result` y pasan a
  `DumpError`; la compatibilidad de los assets no implica compatibilidad
  universal de la API. `serde-wincode 0.1.2` no implementa serialización
  de `i128`/`u128` y wincode 0.6.2 aplica su límite predeterminado de 4 MiB
  a `String` deserializado con ownership.
- La lectura comprimida descomprime completamente en un `Vec` sin límite
  antes de deserializar; la escritura también usa un buffer temporal.
  Finalizar y hacer flush explícito evita ocultar errores de escritura,
  pero no demuestra mejor memoria, velocidad o cancelación en la app.
- Mantener un backport propio exige procedencia, revisión de cada actualización,
  lock reproducible y pruebas diferenciales. Su distribución debe conservar
  MIT de syntect y Apache 2.0 de ambos serializers en los paquetes, con
  comprobación del contenido instalado. Se prefiere una release oficial
  compatible antes de asumir ese mantenimiento sólo para quitar un aviso.

### PEM y dependencias TLS: ruta disponible, fuera del alcance actual

El reemplazo oficial de PEM se describe en las
[releases de rustls-pemfile](https://github.com/rustls/pemfile/releases).
[`axum-server 0.8.0`](https://github.com/programatik29/axum-server/releases/tag/v0.8.0)
ya sustituyó rustls-pemfile por rustls-pki-types y cambia el tipo de servidor
para admitir conexiones genéricas. Para la ruta WebSocket, el
[manifest de tungstenite 0.25.0](https://github.com/snapview/tungstenite-rs/blob/v0.25.0/Cargo.toml)
permite rustls-native-certs 0.8; hay que alinear las versiones compatibles
de tungstenite/tokio-tungstenite desde 0.25 y resolver
[`rustls-native-certs 0.8.1` o posterior](https://github.com/rustls/rustls-native-certs/releases/tag/v%2F0.8.1),
que usa directamente el decodificador PEM de rustls-pki-types. Cambiar sólo
el uso directo de rustls-pemfile deja las rutas transitivas actuales.

La migración puede cambiar qué ocurre cuando sólo una parte de las raíces
es válida: el [cliente TLS de tungstenite 0.25](https://github.com/snapview/tungstenite-rs/blob/v0.25.0/src/tls.rs)
registra errores de carga y usa `add_parsable_certificates`, aceptando las
raíces analizables e ignorando otras. Deben comprobarse certificados mixtos,
almacenes vacíos, `SSL_CERT_FILE`/`SSL_CERT_DIR` y fallos de lectura sin
rebajar la validación TLS. Además, el
[changelog de tungstenite 0.25](https://github.com/snapview/tungstenite-rs/blob/v0.25.0/CHANGELOG.md)
introduce payloads basados en Bytes, un buffer de lectura predeterminado de
128 KiB y cambios de construcción de WebSocketConfig. Estas diferencias
requieren revisar memoria/buffers y conexiones reales de colaboración;
no se implementan ni se dan por verificadas en este pase sin Online e invitaciones.

La migración implementada conserva explícitamente las features nativas de
eframe y selecciona `wayland-csd-adwaita-crossfont` en
[winit 0.30.13](https://github.com/rust-windowing/winit/blob/v0.30.13/Cargo.toml).
Las decoraciones conservan título y controles mediante Fontconfig/FreeType.
`scripts/graphics-deps-verify.py` comprueba las features, la ausencia de los
parsers retirados y la compatibilidad de bindings Direct3D. Las pruebas
headless no sustituyen la validación de fuentes, clipboard, DPI, render y
decoraciones en Windows, Linux y ambos macOS.

Para cerrar cada aviso: revisar el `Cargo.lock` y el árbol de todas las
plataformas/features, ejecutar una auditoría nueva sin ignores, pasar la CI
del candidato y las pruebas nativas del componente migrado. Mantener las
migraciones en commits separados de las correcciones de persistencia y de
los cambios de distribución.

## Acciones de CI y distribución

GitHub señaló la retirada de Node 20 durante la CI del checkpoint. `ci.yml`
y `release.yml` ahora fijan por SHA versiones compatibles con Node 24:
[checkout 5.1.0](https://github.com/actions/checkout/tree/v5.1.0),
[setup-python 6.3.0](https://github.com/actions/setup-python/tree/v6.3.0),
[upload-artifact 6.0.0](https://github.com/actions/upload-artifact/tree/v6.0.0),
[download-artifact 7.0.0](https://github.com/actions/download-artifact/tree/v7.0.0)
y [action-gh-release 3.0.3](https://github.com/softprops/action-gh-release/tree/v3.0.3).
Conservan los inputs de checkout, Python, artefactos y publicación. La acción
de publicación sólo corre para tags, con firmas verificadas; el ensayo manual
no comprueba esa publicación ni sustituye las credenciales reales.

Estas acciones requieren runner 2.327.1 o posterior. El runner observado del
checkpoint usa 2.337.0; la matriz conserva Windows 2025, Ubuntu 24.04 y macOS 15
Intel/Apple Silicon. Se fijó Ubuntu 24.04 también para auditoría y benchmarks
ante la [migración anunciada del alias ubuntu-latest](https://github.blog/changelog/2026-09-17-ubuntu-26-generally-available-and-latest-migration/).
Las ejecuciones de GitHub enlazadas en `DELIVERY-CONTINUATION.md` registran el
estado de validación del candidato y los resultados de estos cambios.
Los workflows manuales de reparación de pases
anteriores no recibieron cambios en esta revisión.
