# Dependencias con avisos de mantenimiento

La auditoría local guardada en `dist/validation-audit.json` usa la base de
RustSec del 3 de octubre de 2026: cero vulnerabilidades conocidas y cuatro
avisos de mantenimiento. Es un resultado de esa consulta, no una garantía
permanente. El lockfile no cambió durante la revisión del 6 de octubre.
No se agregaron ignores para ocultar los avisos.

## Rutas que deben migrarse

| Dependencia en el lockfile | Cómo llega a la app | Trabajo necesario |
| --- | --- | --- |
| `bincode 1.3.3` | `syntect 5.3.0`, usado directamente y por `two-face 0.5.2` | El highlighting carga assets embebidos serializados. Revisar una versión o sustitución que cambie el serializador y los assets juntos; comprobar lenguajes, temas, archivos grandes y tiempos de carga. No es el formato de layout o historial propio de la app. |
| `paste 1.0.15` | `metal 0.31.0` → `wgpu-hal 25.0.2` → `wgpu` → `eframe` | Migrar el stack gráfico compatible con egui y comprobar render, clipboard, terminales y fallbacks en macOS Intel/Apple Silicon. Reemplazar una macro transitiva aisladamente no valida el backend. |
| `rustls-pemfile 2.2.0` | Dependencia directa; también `axum-server` y `rustls-native-certs` a través de WebSocket/TLS | El reemplazo directo por `rustls-pki-types::pem::PemObject` deja las rutas transitivas. La migración completa afecta certificados y conexiones de colaboración: está fuera del pase que excluye Online e invitaciones. |
| `ttf-parser 0.25.1` | `owned_ttf_parser` → `ab_glyph` → `epaint`; además `sctk-adwaita` → `winit` en Linux | Migrar fuentes y revisar todas las features de ventanas. Probar Unicode, fallback, DPI, tamaños de celda y decoraciones en Windows/Linux/macOS. Actualizar egui por sí solo puede conservar la segunda ruta. |

## Evidencia y criterios de cierre

El [manifest de syntect](https://github.com/trishume/syntect/blob/master/Cargo.toml)
mantiene bincode en sus features de parsing y carga de dumps. El
[backend Metal](https://github.com/gfx-rs/metal-rs/blob/master/Cargo.toml)
usa paste; la evolución de
[wgpu-hal](https://github.com/gfx-rs/wgpu/blob/trunk/wgpu-hal/Cargo.toml)
debe evaluarse junto al consumidor eframe, sin asumir compatibilidad con el
stack fijado en este proyecto.

El reemplazo oficial de PEM se describe en las
[releases de rustls-pemfile](https://github.com/rustls/pemfile/releases).
Antes de migrarlo hay que comprobar también las dependencias TLS transitivas,
el formato de certificados existente y las conexiones reales.

El [changelog de egui](https://github.com/emilk/egui/blob/main/CHANGELOG.md)
documenta el cambio de fuentes a skrifa. Sin embargo,
[eframe 0.36.2](https://github.com/emilk/egui/blob/0.36.2/Cargo.toml)
todavía fija winit 0.30.13; sus
[features predeterminadas](https://github.com/rust-windowing/winit/blob/v0.30.13/Cargo.toml)
incluyen las decoraciones Adwaita que usan ab_glyph. Esa actualización no
demuestra por sí misma que desaparezca ttf-parser del lockfile multiplataforma.
Esas versiones se investigaron; no se instalaron ni se probaron aquí.

Para cerrar cada aviso: revisar el `Cargo.lock` y el árbol de todas las
plataformas/features, ejecutar una auditoría nueva sin ignores, pasar la CI
del candidato y las pruebas nativas del componente migrado. Mantener las
migraciones en commits separados de las correcciones de persistencia y de
los cambios de distribución.
