# Progreso de implementación del PLAN-MAESTRO

Estado: **22/23 ítems marcados como completos.** P2.11 ya conserva árbol, foco,
sesión y scrollback por hoja a través de reinicios. Ship-it 7.1 sigue parcial:
el pipeline reproducible existe, pero publicar un release firmado/notarizado y
el cask requiere credenciales y un tap externos. También queda una limitación
del entorno que ya existía antes de este trabajo (`ghostty-vt`).

## P0 — Quick wins
- [x] P0.1 Drag & drop de archivos
- [x] P0.2 Screenshot al agente
- [x] P0.3 Sanitización de session ids
- [x] P0.4 Worktree add robusto (timeout + --no-track)
- [x] P0.5 Escritura durable del layout

## P1 — Completar flujos
- [x] P1.6 Anotaciones por línea en diff → agente
- [x] P1.7 Scrollback ANSI + log incremental
- [x] P1.8 Unread persistente + cooldown de notificaciones
- [x] P1.9 Trash diferido + salvaguardas de borrado
- [x] P1.10 Tabla de providers + resume por id, latest nativo y reemplazo seguro del runtime

## P2 — Capacidades nuevas
- [x] P2.11 Terminal splits — árbol, foco, identidad/runtime y scrollback por
      hoja persistentes; cierre de raíz con promoción segura
- [x] P2.12 Hooks de agente
- [x] P2.13 GitHub in-app vía gh
- [x] P2.14 Quick open unificado

## P3 — Arquitectura
- [x] P3.15 Daemon de PTYs — implementado detrás del feature `daemon`. El bundle
      de macOS activa el feature e incluye el helper; los builds de desarrollo
      sin feature conservan el fallback in-process. **Endurecido por auditoría:**
      protocolo v3 con `client_id`, ownership por cliente y reconciliación
      scoped, singleton del socket con verificación de vida, snapshot
      semántico del grid para reattach caliente, log incremental v2 con seq
      monotónico y rechazo de gaps, persistencia no destructiva con ACKs
      durables, restore de scrollback y SQLite en workers de fondo, prioridad
      de reader para la sesión enfocada, verificación del link `.git` del
      worktree, entrega de startup-input remoto y resume exacto por id para
      todos los providers
- [x] P3.16 Flow control
- [x] P3.17 Linear
- [x] P3.18 Design Mode (extensión browser)

## Ship-it
- [ ] 7.1 Empaquetado — **parcial**: `.app`/DMG con daemon, workflow de
      firma/notarización/publicación y checker de releases con descarga manual;
      faltan credenciales, publicación real del tag/cask e instalación automática
- [x] 7.2 Onboarding (detección de agentes + empty states + overlay)
- [x] 7.3 Perf budgets (asserts duros + benches con comparación +15% en CI)
- [x] 7.4 Smoke E2E (egui_kittest sobre la app real)
- [x] 7.5 Diagnóstico exportable (sin secretos)

## Estado de P2.11

La invariante nueva está cerrada y cubierta: la raíz durable se restaura antes
del árbol, cada hoja conserva su runtime id, el daemon reconcilia todas las
sesiones, los checkpoints/logs se separan por hoja y cerrar la raíz promueve el
controlador sobreviviente. Los layouts anteriores se migran usando la primera
hoja DFS y el runtime id legado de la raíz.

## Limitación del entorno: `ghostty-vt`

Revalidado el 29 de agosto de 2026 en macOS 26.6.2 con Command Line Tools:

- Zig 0.16.0 es rechazado correctamente porque `libghostty-vt-sys 0.1.1`
  requiere Zig 0.15.2.
- Con el binario oficial de Zig 0.15.2, verificado contra el SHA-256 publicado
  por Zig, `cargo check --all-targets --all-features --locked` todavía falla al
  enlazar el build runner de la dependencia. El linker no resuelve símbolos del
  runtime de macOS como `__availability_version_check`, `_abort` y
  `_arc4random_buf`.
- Definir explícitamente `DEVELOPER_DIR` y `SDKROOT` para Command Line Tools no
  cambia el resultado. Este host no tiene una instalación completa de Xcode con
  la cual comprobar una combinación alternativa de SDK/linker.

Por lo tanto, **no hay una validación verde vigente de `ghostty-vt` en este
entorno**. El resultado anterior de `check`/`clippy` con Zig 0.15.2 queda como
histórico y no debe presentarse como reproducido actualmente. El feature sigue
siendo experimental y no forma parte del bundle de producción, que usa el
backend estable y activa solamente `daemon`.

El próximo gate es reproducirlo con una instalación completa y compatible de
Xcode/SDK; si sigue fallando, habrá que actualizar o parchear
`libghostty-vt-sys` antes de incorporar este feature a la matriz de release.

## Cómo verificar el daemon end-to-end

```sh
cargo build --features daemon --bins
./target/debug/mi-terminal          # crea las sesiones en el daemon
# cerrar la app; el daemon y los shells siguen vivos
./target/debug/mi-terminal          # se reengancha a los MISMOS shells
```

En macOS, `scripts/bundle.sh` ejecuta ese build con `--features daemon --bins` y
copia `mi-terminal-daemon` junto al ejecutable de la app. Si hay una
`CODESIGN_IDENTITY`, firma primero el helper y después el bundle.

## Estado de Ship-it 7.1

No hay que interpretar el bundle local como un release publicado. Hoy siguen
pendientes tareas externas y de integración:

- firmar/notarizar con credenciales Apple reales y publicar los artefactos;
- generar el SHA256 final y publicar el cask en un tap real;
- completar la instalación automática: el checker ya usa el repositorio real,
  reconoce el DMG publicado y abre su descarga manual validada.

La secuencia y los límites actuales están documentados en `docs/RELEASE.md`.
