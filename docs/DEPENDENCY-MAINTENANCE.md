# Dependencias con avisos de mantenimiento

La migración gráfica del 7 de octubre de 2026 usa egui/eframe/egui_kittest
0.36.2 y wgpu 30.0.1. La [auditoría del candidato](https://github.com/MauroProto/terminal-canvas/actions/runs/37573217610)
`c7745cb2251c9d6c761842a501883d0f695601a1` pasó sin vulnerabilidades
reportadas y con dos avisos de mantenimiento: `bincode 1.3.3` y
`rustls-pemfile 2.2.0`. No se agregaron ignores. El resultado corresponde
a esa consulta, no es una garantía permanente.

`Cargo.lock` ya no contiene `paste`, `ttf-parser`, `ab_glyph`,
`owned_ttf_parser`, el backend antiguo `metal`, `yaml-rust` ni `block`.
La integración y las comprobaciones nativas se registran por separado en
[GRAPHICS-MIGRATION.md](GRAPHICS-MIGRATION.md).
La auditoría local de `dist/validation-audit.json`, del 3 de octubre,
y la [auditoría de GitHub del 6 de octubre](https://github.com/MauroProto/terminal-canvas/actions/runs/37544616708/job/112545511009)
son evidencia histórica del lockfile anterior, que tenía cuatro avisos.

## Rutas que deben migrarse

| Dependencia en el lockfile | Cómo llega a la app | Trabajo necesario |
| --- | --- | --- |
| `bincode 1.3.3` | `syntect 5.3.0`, usado directamente y por `two-face 0.5.2` | El highlighting carga assets embebidos serializados. Revisar una versión o sustitución que cambie el serializador y los assets juntos; comprobar lenguajes, temas, archivos grandes y tiempos de carga. No es el formato de layout o historial propio de la app. |
| `rustls-pemfile 2.2.0` | Dependencia directa; también `axum-server` y `rustls-native-certs` a través de WebSocket/TLS | El reemplazo directo por `rustls-pki-types::pem::PemObject` deja las rutas transitivas. La migración completa afecta certificados y conexiones de colaboración: está fuera del pase que excluye Online e invitaciones. |

## Evidencia y criterios de cierre

El [manifest de syntect](https://github.com/trishume/syntect/blob/master/Cargo.toml)
mantiene bincode en sus features de parsing y carga de dumps. La
[última release publicada de syntect](https://github.com/trishume/syntect/releases),
consultada el 7 de octubre, sigue siendo 5.3.0. Cambiar sólo la versión de
bincode no elimina la dependencia de ese formato en los assets de two-face;
la sustitución requiere migrar y verificar el resaltador y sus gramáticas juntos.

El reemplazo oficial de PEM se describe en las
[releases de rustls-pemfile](https://github.com/rustls/pemfile/releases).
Antes de migrarlo hay que comprobar también las dependencias TLS transitivas,
el formato de certificados existente y las conexiones reales.

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
