# Progreso de implementación del PLAN-MAESTRO

Estado: **19/19 ítems implementados y verificados.** Queda una desviación
deliberada respecto de la letra del plan (P2.11 T2, explicada al final) y una
limitación del entorno que ya existía antes de este trabajo (`ghostty-vt`).

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
- [x] P1.10 Tabla de providers + resume por id

## P2 — Capacidades nuevas
- [x] P2.11 Terminal splits (ver desviación 1)
- [x] P2.12 Hooks de agente
- [x] P2.13 GitHub in-app vía gh
- [x] P2.14 Quick open unificado

## P3 — Arquitectura
- [x] P3.15 Daemon de PTYs — los terminales viven en el daemon y sobreviven al
      cierre de la app; detrás del feature `daemon`, off por defecto como pedía
      el plan ("para migrar gradual")
- [x] P3.16 Flow control
- [x] P3.17 Linear
- [x] P3.18 Design Mode (extensión browser)

## Ship-it
- [x] 7.1 Empaquetado (.app + dmg + cask + instalador con verificación de firma)
- [x] 7.2 Onboarding (detección de agentes + empty states + overlay)
- [x] 7.3 Perf budgets (asserts duros + benches con comparación +15% en CI)
- [x] 7.4 Smoke E2E (egui_kittest sobre la app real)
- [x] 7.5 Diagnóstico exportable (sin secretos)

## Desviación 1: P2.11 T2, a propósito

El plan pedía que `TerminalPanel` pasara de un `SessionController` a un
`HashMap<LeafId, SessionController>`. El **efecto** buscado ya está: cada hoja
tiene su sesión, su render, su scroll, su input y su scrollback.

Lo que no se hizo es mover la hoja **raíz** adentro del mapa: sigue siendo un
campo directo (`session`) y las demás viven en `leaf_sessions`. Es deliberado —
con la raíz en el mapa, cada acceso pasaría a ser un lookup que puede fallar, y
habría que sostener con `unwrap`/fallback una invariante ("siempre hay al menos
una sesión") que hoy el tipo garantiza gratis. Cambiarlo empeoraría el código
para coincidir con una frase.

## Limitación del entorno: `ghostty-vt`

Verificado hasta donde el entorno permite:

```sh
export PATH="/opt/homebrew/Cellar/zig@0.15/0.15.2/bin:$PATH"
cargo check --features ghostty-vt                              # OK
cargo clippy --all-targets --features ghostty-vt -- -D warnings # OK
```

O sea: **el código de este proyecto compila y pasa clippy** bajo el feature. Dos
cosas del entorno lo bloquean más allá de eso:

1. El `zig` del PATH es 0.16.0 y `libghostty-vt-sys 0.1.1` pide ≤0.15.2. Hay un
   `zig@0.15` en el Cellar, y con eso alcanza para compilar (arriba).
2. Con zig 0.15.2, **linkear los binarios de test** falla, pero no por este
   código: `libghostty-vt-sys` fuerza `-mmacosx-version-min=11.0` y el linker de
   macOS 26 no puede resolver `___dso_handle` en objetos C de `aws-lc-sys` y
   `libmimalloc-sys`. No se arregla sin parchear el build script de una
   dependencia o bajar el toolchain del sistema.

Antes de este trabajo la situación era peor: en el commit base (`60b88a1`) ni
compilaba.

## Cómo verificar el daemon end-to-end

```sh
cargo build --features daemon --bins
./target/debug/mi-terminal          # crea las sesiones en el daemon
# cerrar la app; el daemon y los shells siguen vivos
./target/debug/mi-terminal          # se reengancha a los MISMOS shells
```

Verificado así: una variable seteada en el shell antes de cerrar la app seguía
existiendo después de reabrirla, o sea que sobrevivió el proceso y no solo el
historial.
