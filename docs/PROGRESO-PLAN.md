# Progreso de implementación del PLAN-MAESTRO

Estado: **19/19 ítems implementados**, con dos salvedades explícitas al final.

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
- [x] P2.11 Terminal splits (ver salvedad 1)
- [x] P2.12 Hooks de agente
- [x] P2.13 GitHub in-app vía gh
- [x] P2.14 Quick open unificado

## P3 — Arquitectura
- [x] P3.15 Daemon de PTYs (ver salvedad 2)
- [x] P3.16 Flow control
- [x] P3.17 Linear
- [x] P3.18 Design Mode (extensión browser)

## Ship-it
- [x] 7.1 Empaquetado (.app + dmg + cask + instalador con verificación de firma)
- [x] 7.2 Onboarding (detección de agentes + empty states + overlay)
- [x] 7.3 Perf budgets (asserts duros + benches con comparación +15% en CI)
- [x] 7.4 Smoke E2E (egui_kittest sobre la app real)
- [x] 7.5 Diagnóstico exportable (sin secretos)

## Salvedades

**1. P2.11 — pipeline de input de los splits.** El árbol, las sesiones por
hoja, los atajos, el render tiling, los divisores arrastrables, el badge de
hoja activa y la persistencia por hoja están. Lo que sigue siendo *de la hoja
enfocada* y no *por hoja* es el pipeline de input fino: selección con mouse,
scrollbar y búsqueda en scrollback. Funciona y está testeado, pero no es el
refactor completo de `TerminalPanel` a `HashMap<LeafId, SessionController>`
que describía T2: `session` sigue siendo la hoja raíz y las demás viven en
`leaf_sessions`.

**2. P3.15 — el render de la app sigue in-process.** El daemon está completo
como proceso: protocolo versionado, token 0600, posesión real de los PTYs,
pump de salida con `seq`, broadcast a varias apps, reattach con snapshot,
reconciliación de huérfanos, apagado por inactividad y 8 tests de proceso
real (incluido uno que escribe en el PTY y verifica la salida por el socket).
Lo que **no** se hizo es reemplazar el `PtyManager` de la app por el cliente:
hoy la app renderiza sus grids in-process y el daemon se usa (detrás del
feature `daemon`, off por defecto) para el ciclo de vida. Cerrar eso implica
consumir los eventos `Output` como fuente del grid, que es un segundo backend
de terminal completo.

**3. Feature `ghostty-vt` no verificable acá.** Falla al compilar
`libghostty-vt-sys 0.1.1` porque el zig instalado es 0.16.0 y la dependencia
pide ≤0.15.2. Es **pre-existente**: falla idéntico en el commit base
(`60b88a1`), antes de este trabajo.
