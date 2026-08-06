# Plan maestro de terminalcanvas

> Qué construir, en qué orden, con qué diseño — con lo mejor de Orca como referencia estudiada.

> Documento de trabajo. Fuente: clon completo de `stablyai/orca` (12.422 archivos,
> Electron/TypeScript) explorado a fondo el 2026-08-05, más el análisis de nuestro
> propio código. Cada afirmación sobre Orca cita el archivo real del repo de ellos.
> Cada plan referencia lo que ya existe en el nuestro.
>
> Orca es MIT. Copiar diseño y mecanismos es legal y explícitamente bienvenido.

---

## Índice

- [0. Dónde estamos nosotros hoy](#0-dónde-estamos-nosotros-hoy)
- [1. Tu wishlist, uno por uno](#1-tu-wishlist-uno-por-uno)
  - [1.1 Terminal splits](#11-terminal-splits)
  - [1.2 Scrollback que sobrevive reinicios (nivel Orca)](#12-scrollback-que-sobrevive-reinicios-nivel-orca)
  - [1.3 Anotar diffs y mandarlos al agente](#13-anotar-diffs-y-mandarlos-al-agente)
  - [1.4 Arrastrar archivos al agente](#14-arrastrar-archivos-al-agente)
  - [1.5 Cualquier agente CLI](#15-cualquier-agente-cli)
  - [1.6 GitHub y Linear dentro de la app](#16-github-y-linear-dentro-de-la-app)
  - [1.7 Design Mode](#17-design-mode)
- [2. Arquitectura mayor que hay que adoptar](#2-arquitectura-mayor-que-hay-que-adoptar)
- [3. Mejoras medianas, listas para ejecutar](#3-mejoras-medianas-listas-para-ejecutar)
- [4. Cosas chicas de código (quick wins)](#4-cosas-chicas-de-código-quick-wins)
- [5. Roadmap priorizado](#5-roadmap-priorizado)
- [6. Estimación de tiempo y costo](#6-estimación-de-tiempo-y-costo)
- [7. Lo que al plan le faltaba (autocrítica)](#7-lo-que-al-plan-le-faltaba-autocrítica)
- [8. Protocolo de ejecución](#8-protocolo-de-ejecución-cómo-se-trabaja-cada-ítem)

---

## 0. Dónde estamos nosotros hoy

Para no planear en el aire: esto ya existe en terminalcanvas (38 commits recientes):

| Área | Estado nuestro |
|---|---|
| Paneles de terminal en canvas | Sí: flotantes, snap, minimizar, taskbar |
| Scrollback persistente | Básico: texto plano por panel, 256 KB, replay con marcador |
| Resume de agentes | Sí: `--continue` al restaurar panel + selector `Ctrl+Shift+R` que lee `~/.claude/projects` |
| Code review con diff | Sí: parser unified diff, UI virtualizada, feedback al agente (texto libre, no por línea) |
| Worktrees | Crear/listar/borrar (borrado en worker) |
| Visor de código | Sí: dockeado a la derecha, syntect (213 lenguajes), worker thread |
| File explorer | Sí: árbol lazy en sidebar |
| Broadcast a N terminales | Sí (`Ctrl+Shift+Enter`) |
| Estado de agente | OSC 9999 in-band + heurística de texto |
| Notificaciones | Transición a atención → notificación del SO |
| Detección de muerte sucia | Sí: run marker + runs.log |
| Config | `config.toml` + diálogo en vivo |
| Splits dentro de un panel | **No** |
| Anotaciones por línea en diff | **No** |
| Drag & drop de archivos | **No** |
| GitHub/Linear in-app | **No** |
| Design Mode | **No** |
| Daemon (PTYs sobreviven a la app) | **No** |

---

## 1. Tu wishlist, uno por uno

### 1.1 Terminal splits

**Qué hace Orca.** Splits infinitos dentro de una tab, con foco navegable por
teclado (`terminal.split*`, `terminal.focus*Pane` en `src/shared/keybindings.ts`)
y layouts persistidos por tab (`terminalLayoutsByTabId` dentro de
`workspaceSession`, en `src/main/persistence.ts`).

**Qué tenemos.** Paneles independientes en el canvas, pero un panel = un
terminal. No hay subdivisión interna.

**Plan paso a paso.**
1. **Modelo**: árbol binario de layout por panel. Nuevo módulo
   `src/terminal/split_tree.rs`:
   ```
   enum SplitNode { Leaf(SessionId), HSplit{ratio, a, b}, VSplit{ratio, a, b} }
   ```
   Lógica pura: `split(leaf, dir)`, `close(leaf)` (el hermano absorbe),
   `focus_next/prev`, `resize(ratio)`. Todo testeable sin UI (mismo criterio que
   `file_tree.rs`).
2. **Render**: `TerminalPanel::show` hoy dibuja un grid; pasa a recorrer el
   árbol y asignar un `Rect` por hoja. Cada hoja ya es un `SessionController`
   (esa abstracción ya la tenemos y es exactamente lo que hace falta).
3. **Input**: el foco interno del panel apunta a una hoja; `handle_input`
   enruta a esa hoja. Atajos: `Cmd+D` split vertical, `Cmd+Shift+D` horizontal,
   `Cmd+[`/`Cmd+]` ciclar hojas (mismo espíritu que Orca).
4. **Persistencia**: serializar el árbol en `PanelState` (hoy guarda un solo
   terminal). El scrollback por hoja ya funciona porque el store va por
   session/panel id — pasar la clave a `(panel_id, leaf_id)` como hace Orca con
   su hash `sha256(tabId\0leafId)` en `terminal-scrollback-snapshots.ts`.
5. **Divisores arrastrables**: hit-test sobre el borde compartido, actualizar
   `ratio` con clamp (0.15..0.85).

**Esfuerzo**: mediano (3–5 tandas). **Riesgo**: bajo — no toca el runtime de PTY,
solo layout. Sobre "WebGL rendering": nuestro equivalente ya existe y es mejor —
renderizamos nativo con wgpu, y ya hicimos el run-batching de glifos.

---

### 1.2 Scrollback que sobrevive reinicios (nivel Orca)

**Qué hace Orca** (lo más sofisticado que tienen; `src/main/daemon/*`):
- Un **emulador headless espejo** por sesión (`@xterm/headless` +
  `addon-serialize` en `headless-emulator.ts`), scrollback 5000 líneas.
- **Dos niveles en disco** por sesión (`userData/terminal-history/<id>/`):
  - `checkpoint.json`: snapshot ANSI serializado, escrito tmp→rename, con
    número de `generation`. Incluye: cursor **absoluto** (el relativo queda
    corrido tras un wrap pendiente — `terminal-serialize-absolute-cursor.ts`),
    la **cola de escape parcial** del parser (un snapshot sin ella renderiza
    basura literal al restaurar), modos (mouse/bracketed paste/alt-screen) como
    secuencias de rehidratación, cwd/título vía OSC.
  - `output.log`: log binario incremental con framing (`'OCKL'` + generation +
    frames `output|resize|clear` con seq). Un crash corta el último frame → el
    decoder trunca en el último frame completo. Gap en seq → log inválido.
- **Cadencia**: append cada **5 s solo si hay sesiones sucias** (dirty-gated,
  cero wakeups idle); checkpoint completo raro, con cooldown de **45 s**; log
  cap 5 MB → checkpoint y reset con generation+1.
- **Restauración**: prefiere replay del log (más fresco que el checkpoint),
  verifica `log.generation == checkpoint.generation`, y reproduce con
  presupuesto de 64 KB/turno para no congelar la UI.
- Regla citada textual: *"un prefijo stale es mejor que un agujero indetectable"*.

**Qué tenemos.** Texto plano por panel (256 KB), guardado en autosave (2 s) y
al salir, replay con CRLF + separador. Funciona, pero: pierde colores/estilos,
pierde el estado del cursor y los modos, y reescribe el archivo entero cada vez.

**Plan por etapas (cada una útil por sí sola).**
1. **Guardar ANSI, no texto plano.** Nuestro grid de alacritty tiene los
   atributos por celda; al exportar, emitir SGR mínimos (fg/bg/bold) además del
   texto. El replay ya pasa por un parser ANSI, así que los colores se
   restauran gratis. *(chico)*
2. **Log incremental en vez de reescritura.** Duplicar el diseño de Orca en
   `scrollback_store`: `checkpoint` (lo de hoy) + `append.log` con frames
   `output/resize/clear` y seq. Trunca la cola rota, detecta gaps, generation.
   La cadencia dirty-gated de 5 s ya la puede manejar nuestro autosave. *(mediano)*
3. **Records de resize/clear en el stream.** Cita de ellos: el replay debe
   refluir en el punto exacto; si el resize no está en el log como record, el
   texto reflowea mal. *(chico, parte de 2)*
4. **Subir el límite con retención global.** Orca retiene 10.000 sesiones con
   heap oldest-first (`terminal-history-restorable-retention.ts`). Nosotros:
   límite por panel (p.ej. 2 MB) + GC del directorio que ya tenemos (prune). *(chico)*

**Esto resuelve tu "persistencia infinita"** con RAM acotada: disco ilimitado
por diseño, memoria fija.

---

### 1.3 Anotar diffs y mandarlos al agente

**Qué hace Orca.** Notas por línea o rango sobre el diff, persistidas **en la
metadata del worktree** (sobreviven restart, viajan con el worktree), y un menú
"enviar" que las entrega al agente como prompt.

Detalles que valen oro (`src/shared/types.ts:784`,
`src/renderer/src/store/slices/diffComments.ts`,
`src/shared/diff-comments-format.ts`):
- `DiffComment { filePath, startLine?, lineNumber, body, sentAt?, scope }`.
  `lineNumber == 0` significa "comentario al archivo".
- **`sentAt` marca "ya entregado"; editar el body borra `sentAt`** → la nota se
  re-encola sola.
- El clear posterior a enviar compara contra un **snapshot de entrega**: una
  nota editada después del envío es una nota nueva y no se borra.
- Formato de prompt determinístico (es el contrato con el agente):
  ```
  File: src/foo.ts
  Lines: 7-12
  User comment: "el texto, escapado"
  ```
- El menú de envío lista los agentes vivos del worktree con su estado y
  re-valida elegibilidad **al momento del click**; ofrece "New agent" que lanza
  uno y entrega el prompt cuando el TUI está listo (`submit-after-ready`).

**Qué tenemos.** El 70% del terreno: code review con diff parseado y
virtualizado, y `send_prompt_to_panel` con bracketed paste (lo usa el feedback
libre actual). Falta el modelo de notas por línea.

**Plan paso a paso.**
1. `src/orchestration/diff_notes.rs`: struct `DiffNote` calcada del modelo de
   Orca (con `sent_at: Option<DateTime>`), colección por repo_root, persistida
   en un JSON junto al layout. Pura + tests (incluido: editar borra `sent_at`;
   clear compara snapshot).
2. `format_note()` con el formato determinístico de arriba. Test que fija el
   formato exacto (es un contrato, no un detalle).
3. UI en `code_review_ui`: click en el gutter del diff → editor inline debajo
   de la línea (una fila extra en el listado virtualizado; ya insertamos filas
   sintéticas para headers de archivo, es el mismo mecanismo).
4. Botón "Enviar N notas" → reusar el selector de destinos del broadcast
   (`broadcast_ui` ya lista paneles vivos con estado).
5. Al confirmar entrega: marcar `sent_at` y filtrarlas de la vista.

**Esfuerzo**: mediano (2–3 tandas). Es la feature con mejor relación
valor/esfuerzo de toda la lista, porque completa un flujo que ya existe.

---

### 1.4 Arrastrar archivos al agente

**Qué hace Orca.** Arrastrás un archivo o imagen a un prompt de agente y se
inserta la referencia.

**Qué tenemos.** Nada — pero egui trae el 90%: `ctx.input(|i| i.raw.dropped_files)`
entrega los paths soltados sobre la ventana, sin ninguna dependencia nueva.

**Plan paso a paso.**
1. En `begin_frame`: leer `dropped_files`. Hit-test contra el panel bajo el
   puntero (ya existe `hit_test` por panel para el mouse).
2. Si cayó sobre un terminal: escribir el path **shell-escapado** (comillas
   simples con escape de `'`) precedido de un espacio — exactamente lo que hace
   Terminal.app. Si el panel corre un agente y son varios archivos, unirlos en
   una línea.
3. Feedback visual durante el drag: `ctx.input(|i| i.raw.hovered_files)` para
   resaltar el panel destino con el ring de foco existente.
4. Extra barato: soltar un archivo sobre el **visor de código** lo abre
   (llamada a `open_file_viewer` que ya existe).

**Esfuerzo**: chico (1 tanda). Quick win — hacerlo primero.

---

### 1.5 Cualquier agente CLI

**Qué hace Orca.** Todo agente es "un comando en una terminal" + una tabla de
conocimiento por proveedor. Lo importante está en
`src/shared/agent-session-resume.ts`:

| Agente | Resume |
|---|---|
| claude | `claude --resume <id>` |
| codex | `codex resume <id>` (subcomando, no flag) |
| gemini / droid / grok / devin | `--resume <id>` |
| opencode | `--session <id>` |
| pi | `--session <transcriptPath>` |

Y sanitización de ids antes de usarlos como argv: máx 512 chars, sin control
chars, **rechazar ids que empiezan con `-`** (inyección de flags).

**Qué tenemos.** 5 proveedores (`AgentProvider` en `orchestration/manager.rs`)
con detección por texto, `resume_command()` con `--continue` para
claude/opencode, y `Unknown` como fallback genérico que ya permite correr
cualquier CLI.

**Plan paso a paso.**
1. Ampliar la tabla de `resume_flag()` con lo verificado de Orca (codex usa
   subcomando: requiere que `resume_command` soporte insertar antes de los
   flags). Gemini: `--resume <id>` — pero solo con id capturado, no a ciegas.
2. Portar la sanitización de ids (512 chars, control chars, guion inicial) a
   `agent_sessions.rs`. Test con id malicioso `-rf /`.
3. Sumar providers al enum con su comando de lanzamiento y color de taskbar
   (hoy: 5; Orca: ~30). Empezar por los que uses: cursor-agent, copilot, goose.
4. La captura del **session id real** (no "la más reciente") viene con los
   hooks de la sección 2.2 — ahí el resume pasa de "probablemente la última" a
   "exactamente esta".

**Esfuerzo**: chico por proveedor. La tabla es conocimiento, no código.

---

### 1.6 GitHub y Linear dentro de la app

**Qué hace Orca.** Browsea PRs, issues y boards in-app; desde cualquier issue
abre un worktree ("open a worktree from any task"). Vincula issue/PR al
worktree (`WorktreeMeta` guarda linked issue/PR) y la paleta matchea por número
o título de PR.

**Qué tenemos.** Worktrees y code review propios; nada de GitHub/Linear.

**Plan realista para nativo (sin embeber Chromium).**
1. **Fase A — `gh` CLI como backend** *(mediano)*: `gh pr list/view --json` y
   `gh issue list/view --json` desde un worker (mismo patrón que `DiffLoader`).
   Vista nueva "Tasks" en el sidebar: lista de issues/PRs del repo del
   workspace, con estado y branch. Requiere `gh auth` hecho por el usuario —
   cero manejo de tokens nuestro. Orca mismo aconseja *"be mindful of the gh
   rate limit — batch requests"* (su `AGENTS.md`).
2. **Fase B — issue → worktree** *(chico una vez que A existe)*: botón "Start
   work": crea worktree `issue-<n>-<slug>` (código ya existente) y lanza el
   agente con un prompt inicial que incluye título + body del issue.
3. **Fase C — Linear** *(mediano)*: API GraphQL con token personal en
   `config.toml`. Misma vista, otra fuente.
4. **Vincular al panel**: guardar `linked_issue` en la metadata del worktree
   (como `WorktreeMeta` de Orca) y mostrarlo en el badge del panel.

**Esfuerzo**: mediano-grande total, pero la Fase A sola ya cambia el flujo diario.

---

### 1.7 Design Mode

**Qué hace Orca.** Ventana Chromium embebida; click en cualquier elemento de tu
web app → manda HTML + CSS + screenshot recortado al prompt del agente.

**Evaluación honesta para nosotros.** Es la única feature de tu lista que pelea
contra nuestra arquitectura: exige un motor de browser embebido. Electron lo
trae gratis; en Rust significa `wry`/WebView nativo (WKWebView en macOS) dentro
de una ventana aparte — posible, pero es un proyecto en sí.

**Camino pragmático en 3 escalones.**
1. **Hoy, casi gratis** *(chico)*: comando "Attach screenshot to agent" — en
   macOS `screencapture -i` (selección interactiva del usuario) → guardar a
   tmp → escribir el path al prompt del agente. Claude Code lee imágenes por
   path. Cubre el 60% del valor real (mostrarle al agente "esto está mal").
2. **Puente por extensión de browser** *(mediano)*: mini extensión (manifest
   v3) con un content script: alt+click en un elemento → POST a
   `127.0.0.1:<puerto>` de la app (servidor axum ya tenemos en collab) con
   `outerHTML`, estilos computados y `getBoundingClientRect`; la app lo
   convierte al prompt. Sin embeber nada; funciona con el Chrome real del
   usuario, donde de verdad corre su app.
3. **WebView embebido con `wry`** *(grande, sólo si 2 se queda corto)*: ventana
   secundaria con WKWebView + script inyectado de picking. Evaluar recién
   cuando el resto del roadmap esté hecho.

---

## 2. Arquitectura mayor que hay que adoptar

### 2.1 Daemon de PTYs — los agentes sobreviven a la app

**El problema que resuelve**: hoy, si la app muere (crash, update, kill), mueren
los shells y los agentes con ella. Es tu queja original de "no puede cerrarse y
perder todo".

**Cómo lo hace Orca** (`src/main/daemon/`): un proceso Node **detached** es el
dueño de los PTYs; Electron es un cliente que se conecta por unix socket. La
app puede morir y volver; el daemon sigue con todo corriendo y al volver hace
"reattach caliente" (snapshot + cola de bytes deduplicada por seq).

Detalles de robustez que hay que copiar tal cual:
- **Versión de protocolo en el nombre del socket**
  (`daemon-v{N}.sock`): un daemon viejo jamás atiende a un cliente nuevo
  incompatible.
- Token de auth en archivo `0600`; pid-file con nonce + start-ticks (identidad
  exacta de la encarnación, no solo el pid reciclable).
- **Adoption timeout**: daemon nunca adoptado en 2 min se auto-retira.
- Al morir el daemon: la UI lo detecta (`ECONNREFUSED`), respawnea y hace cold
  restore desde disco — el usuario ve el historial aunque el proceso murió.

**Para nosotros**: binario `mi-terminal-daemon` (mismo repo, ya tenemos
`src/bin/collab-broker.rs` como precedente de segundo binario). Protocolo
NDJSON sobre unix socket, mensajes: `spawn/attach/write/resize/kill/list`.
`PtyManager` ya abstrae attach/detach — es el lugar natural del corte.

**Esfuerzo**: grande (la tanda más grande del roadmap). **Cuándo**: después de
1.2, porque el cold restore reutiliza el checkpoint+log.

### 2.2 Estado de agente por hooks (no por scraping)

**Cómo lo hace Orca** (`src/main/agent-hooks/server.ts`,
`src/main/claude/hook-service.ts`): servidor HTTP en `127.0.0.1:<efímero>` con
token; instala hooks en `~/.claude/settings.json` (eventos `Stop`,
`UserPromptSubmit`, `PermissionRequest`, `PreToolUse`…) que hacen `curl` al
server con el payload. Identidad del pane por env vars inyectadas al PTY
(`ORCA_PANE_KEY`, etc.).

El truco que lo hace robusto: puerto+token viven en un **endpoint file** que
cada ejecución del hook **re-sourcea** — un agente que sobrevive a un reinicio
de la app reporta al server nuevo sin reinstalar nada.

**Qué nos da**: estado exacto (`working/waiting/done`), el **session_id real**
en cada evento (resume exacto, no "la última"), y el prompt del turno para
mostrar en el sidebar. Nuestra heurística OSC 9999 + títulos queda como
fallback (Orca también mantiene un fallback por proceso foreground).

**Para nosotros**: axum ya está en el árbol de dependencias (collab). Endpoint
`POST /hook/claude`; escribir los hooks en `settings.json` con merge cuidadoso
(preservar hooks del usuario; Orca marca los suyos como "managed" para poder
actualizarlos). Env vars al spawn: ya controlamos el `CommandBuilder`.

**Esfuerzo**: mediano. **Valor**: altísimo — es la columna vertebral de todo lo
"multi-agente serio".

### 2.3 Flow control en cadena (fluidez bajo carga)

Orca tiene 4 etapas; para nativo nos aplican dos:

1. **Pausar el PTY en el kernel** cuando el consumidor se atrasa:
   `node-pty pause()/resume()` con watermarks 256 KB / 32 KB e histéresis. En
   Rust: dejar de leer del fd (el pipe se llena y el hijo se bloquea en
   `write()` — backpressure gratis del kernel). **Failsafe de 5 s de
   auto-resume** para que un resume perdido nunca cuelgue un shell — esa regla
   es sagrada.
2. **Carril interactivo reservado**: el eco del tecleo del panel enfocado gana
   siempre al bulk de los paneles ocultos (Orca reserva 256 KB + ventana de
   100 ms). Nuestro scheduler de render (`render_qos`) ya distingue paneles
   enfocados; extenderlo al drenado del PTY.

**Esfuerzo**: mediano. Atacarlo cuando notes que `yes` o un build largo en un
panel degrada el tecleo en otro.

---

## 3. Mejoras medianas, listas para ejecutar

### 3.1 Borrado de worktrees con trash diferido
`src/main/worktree-trash.ts` entero es portable: `git worktree remove` de un
árbol con `node_modules` = 8–35 s de espera. Solución: **rename** a
`.orca-worktree-trash/wt-<epoch>-<nonce>` (misma unidad, instantáneo), retornar
ya, borrar en background **serializado** (un delete a la vez), y sweep al boot
de restos que matcheen el patrón. Nosotros ya borramos en worker; falta el
rename-first y el sweep. *(chico)*

### 3.2 Salvaguardas de borrado
`worktree-removal-safety.ts`: deny-list (repo, home, raíz de volumen,
contenedores), rechazar si contiene otro worktree registrado, y la regla *"la
forma del path no es autoridad"* — para borrar un directorio huérfano hay que
**probar** la relación con el repo (leer el archivo `.git`) o tener provenance
persistida de que lo creamos nosotros. Test obligatorio por cada regla. *(chico)*

### 3.3 Limpieza de branch post-worktree
`git-branch-cleanup.ts`: `-d` primero; si falla, probar con
`git merge-tree --write-tree` que un **squash-merge** no dejó cambios sin
mergear y recién ahí `-D`; si nada prueba seguridad, devolver
`preservedBranch {name, head}` y que la UI ofrezca el force con el head
esperado. *(chico)*

### 3.4 GitCapabilityCache
`src/shared/git-capability-cache.ts`: probe optimista de flags modernos de git,
memoria de "unsupported" con **retry cada 30 min** (detecta upgrade de git sin
reiniciar), y probe coalescido (N llamadas concurrentes esperan al primero). En
Rust: `HashMap<Capability, State>` + `Mutex` por probe. Aplicarlo a
`worktree list -z` y `merge-tree --write-tree`. *(chico)*

### 3.5 Notificaciones nivel Orca
De `use-notification-dispatch.ts` y `src/main/ipc/notifications.ts`:
- **Cooldown de 5 s por workspace** (no por fuente): agente-terminó y bell
  suelen llegar en el mismo chunk; mostrar solo la primera.
- **Chequear liveness al despachar**: cualquier notificación cuyo PTY ya no
  vive es fantasma — un solo guard en el dispatcher caza todos los timers
  stale, en vez de cancelar cada uno.
- **Unread persistente** por panel con modelo "show until interact": se limpia
  con keystroke/click en el panel, no con solo mirarlo. Punto naranja en la
  taskbar. Sobrevive restart (a la metadata del workspace).
- Id estable de notificación por `(panel, state_started_at)` para **reemplazar**
  la notificación anterior del mismo evento, no apilar. *(mediano, alto valor diario)*

### 3.6 Quick open con intención
`cmd-j/palette-results.ts` y `order-empty-query-worktrees.ts`:
- **Reglas ordinales antes del fuzzy score**: query == verbo exacto de acción >
  keyword exacto de settings > prefijo de verbo > … > token score. La intención
  le gana al score.
- **Query vacío = recencia de foco**, y "visitado siempre le gana a no visitado
  aunque el otro tenga actividad más nueva" (presencia primero, valor después).
- Top-K con **min-heap** (50 resultados), no sort total.
- Bonus de boundary (match tras `/`, `.`, `-`) y bonus fuerte si el filename
  contiene el query completo.
Nuestro quick open ya es async; esto es mejorar el ranking y sumar fuentes
(comandos de la paleta + paneles + archivos en una sola superficie). *(mediano)*

### 3.7 Persistencia durable de verdad
`durable-file-write.ts` + `persistence.ts`:
- write tmp → **fsync del archivo antes del rename** (un rename que aterriza
  primero puede exponer un archivo de longitud 0) → rename → fsync del
  directorio.
- **Ring de 5 backups** `.bak.0..4` rotados con ≥1 h de espaciado (snapshots en
  momentos distintos, no 5 copias casi iguales); al cargar, si el principal no
  parsea, probar slot por slot.
- **Hash no-op**: si el estado no cambió, no reescribir el archivo multi-MB.
- Debounce 1 s con **max-wait 5 s** (nuestro autosave de 2 s está bien, falta
  el fsync y el ring).
Aplicar a `layout.json` y `config.toml`. *(chico-mediano)*

---

## 4. Cosas chicas de código (quick wins)

Cada una es ≤1 tanda y viene de un archivo concreto de Orca:

1. **Sanitizar session ids como argv** (`agent-session-resume.ts`): 512 chars
   máx, sin control chars, rechazar `-` inicial. Ya tenemos el lugar exacto:
   `agent_sessions.rs`.
2. **`.worktreeinclude`** (`worktree-include-file.ts`): archivo en la raíz del
   repo listando paths gitignorados (`.env`, `.vscode/`) que se copian a cada
   worktree nuevo. Solo literales, verificados con `git check-ignore`, caps de
   256 KB / 1000 entradas. Los agentes en worktrees fallan sin el `.env` — esto
   lo arregla.
3. **Timeout en `git worktree add`** (180 s, `WORKTREE_ADD_TIMEOUT_MS`): un
   stall de OneDrive/NFS no debe colgar el spawn para siempre.
4. **`--no-track` + `push.autoSetupRemote`** al crear worktrees: heredar
   upstream de la base hace que `git status` mienta "behind by N" antes del
   primer push.
5. **Feedback por duración** (STYLEGUIDE): 0–100 ms nada, 100 ms–1 s disabled,
   1–3 s spinner, 3 s+ etiquetas de etapa. Y **defer del spinner ~200 ms**: lo
   local no parpadea, lo remoto sí informa. Aplicar a: carga de diff, listado
   de worktrees, resaltado de sintaxis.
6. **Cancel nunca es destructive** y sin chips de teclado en el back-out path
   (STYLEGUIDE). Auditar nuestros diálogos (settings/broadcast/resume).
7. **3 niveles de sombra exactos** (hairline border / xs+border / flotante).
   Hoy mezclamos; unificar en `theme/colors.rs` como constantes con nombre.
8. **Comentarios "Why:" con número de issue** en cada guarda no obvia — Orca lo
   hace en TODO el codebase y es lo que lo hace navegable. Ya escribimos así;
   formalizarlo como convención en CLAUDE.md/AGENTS.md nuestro.
9. **Nombres de archivo por concepto, no por rol**: prohibido `utils.rs`
   nuevo (su AGENTS.md: "helpers/utils son vertederos"). Ya cumplimos casi
   todo; `utils/mod.rs` nuestro es candidato a repartirse.
10. **Cooldown de notificaciones deduplicado por origen compartido** (ver 3.5).
11. **Cap de bytes en queries de búsqueda** (2 KB, `isQuickOpenQueryTooLarge`):
    un paste accidental de un archivo entero en el fuzzy no debe colgar el
    frame.
12. **`endedAt == null` como señal de shutdown sucio** en metadata por sesión:
    ya lo tenemos a nivel app (run marker); replicarlo **por sesión de
    terminal** cuando llegue el checkpoint+log (1.2).

---

## 5. Roadmap priorizado

El criterio: primero lo que completa flujos que ya tenemos a medias, después lo
que abre capacidades nuevas, al final lo que exige arquitectura nueva.

### P0 — Quick wins (1 tanda c/u, hacer ya)
| # | Qué | De dónde |
|---|---|---|
| 1 | Drag & drop de archivos al terminal/agente | §1.4 |
| 2 | Screenshot al agente (`screencapture -i` → path al prompt) | §1.7 esc. 1 |
| 3 | Sanitización de session ids | §4.1 |
| 4 | Timeout + `--no-track` en worktree add | §4.3, §4.4 |
| 5 | fsync + ring de backups en layout.json | §3.7 |

### P1 — Completar flujos existentes (2–3 tandas c/u)
| # | Qué | De dónde |
|---|---|---|
| 6 | **Anotaciones por línea en el diff** → agente | §1.3 |
| 7 | Scrollback ANSI (colores) + log incremental | §1.2 |
| 8 | Unread persistente + cooldown de notificaciones | §3.5 |
| 9 | Trash diferido + salvaguardas de borrado de worktrees | §3.1–3.3 |
| 10 | Tabla completa de providers + resume por id | §1.5 |

### P2 — Capacidades nuevas (3–5 tandas c/u)
| # | Qué | De dónde |
|---|---|---|
| 11 | **Terminal splits** | §1.1 |
| 12 | Hooks de agente (HTTP local + settings.json) | §2.2 |
| 13 | GitHub in-app vía `gh` (Fase A+B: issues → worktree) | §1.6 |
| 14 | Quick open con reglas ordinales + fuentes mezcladas | §3.6 |

### P3 — Arquitectura (proyectos)
| # | Qué | De dónde |
|---|---|---|
| 15 | **Daemon de PTYs** (agentes sobreviven a la app) | §2.1 |
| 16 | Flow control con pausa de PTY + carril interactivo | §2.3 |
| 17 | Linear in-app | §1.6 fase C |
| 18 | Design Mode por extensión de browser | §1.7 esc. 2 |

**Dependencias**: 15 depende de 7 (cold restore usa checkpoint+log). 12
potencia a 10 (resume por id exacto) y a 8 (estados sin heurística). 6 reusa el
selector de 10/broadcast.

---

---

## 6. Estimación de tiempo y costo

**Base de la estimación** (no inventada): la velocidad real observada en las
sesiones que produjeron los últimos 38 commits de este repo. Una "tanda" =
una unidad de trabajo con el estándar completo: implementación + tests +
clippy en cero + verificación + commit. Una tanda real tarda entre 20 y 60
minutos de reloj y cuesta entre USD 3 y 8 de uso de API (las tandas con
exploración o depuración visual son las caras; las de código puro, las
baratas). Si el agente corre con suscripción (Max/Team) en vez de API por
token, el costo marginal es ~0 y solo cuenta el tiempo.

| Fase | Contenido | Tandas | Tiempo de agente | Costo API (USD) |
|---|---|---|---|---|
| **P0** | 5 quick wins (drag&drop, screenshot→agente, sanitización, worktree add robusto, fsync+backups) | 5 | 2–4 h | 20–40 |
| **P1** | Anotaciones en diff, scrollback ANSI+log, unread+notifs, trash de worktrees, providers | 9–11 | 5–9 h | 40–80 |
| **P2** | Splits, hooks de agente, GitHub vía gh, quick open unificado | 11–15 | 7–12 h | 55–110 |
| **P3** | Daemon de PTYs, flow control, Linear, Design Mode (ext. browser) | 12–17 | 8–14 h | 70–130 |
| **Total** | Todo el plan | **37–48** | **22–39 h** | **185–360** |

**En calendario**: el tiempo de agente no es tiempo corrido — hay revisión
humana entre tandas. A un ritmo sostenible de 2–4 tandas por día:

- **P0 completo: 1–2 días.**
- **P0+P1 (la app se siente otra): ~1 semana.**
- **Todo el plan: 3–5 semanas.**

**Dónde está el riesgo de desvío** (los números pueden crecer acá y solo acá):

1. **El daemon (P3.15)** es la única pieza con incertidumbre real de diseño en
   Rust nativo; presupuestado 5–8 tandas, podría ser 10 si el reattach caliente
   pelea con alacritty. Mitigación: su dependencia (checkpoint+log) se hace
   antes y ya deja el cold restore funcionando sin daemon.
2. **Hooks de agente (P2.12)**: depende del formato de `~/.claude/settings.json`
   y equivalentes; si un CLI cambia su esquema, hay una tanda extra de ajuste.
3. **Verificación visual**: cuando haga falta mirar la pantalla (splits, diff
   notes), las tandas cuestan ~30% más por el ciclo compilar-lanzar-capturar.

Lo que NO puede pasar con este plan: quedar a mitad de camino con algo roto.
Cada fila de la tabla deja la app en un estado mejor y estable — el orden está
elegido para que cortar en cualquier punto sea seguro.

---

## 7. Lo que al plan le faltaba (autocrítica)

Revisión honesta: las secciones 1–5 cubren el **producto**, pero un proyecto
top necesita además la capa de **ship it**. Esto faltaba y ahora es parte del
plan:

### 7.1 Empaquetado y distribución (P1 — antes de lo que parece)
Hoy la app es un binario que se lanza desde una terminal de desarrollo. Varios
problemas que sufrimos (la ventana perdida en otro Space, el foco que no se
puede pedir por AppleScript) existen porque **no es un `.app` bundle real**.
- `.app` con `cargo-bundle` o script propio: Info.plist, icono, categoría.
- Firma + notarización de macOS (sin esto, Gatekeeper asusta a cualquier
  usuario que no seas vos).
- Canal de updates: ya existe `update.rs` (checker); falta el instalador.
  Referencia: el updater de Orca (`src/main/updater.ts`) — check cada 24 h,
  backoff 1 h→6 h, y difiere el quit hasta que el installer está listo.
- Homebrew cask propio (Orca lo hace en su repo `Casks/`).
*Estimado: 3–4 tandas.*

### 7.2 Experiencia de primer arranque (P1)
Nada en el plan cubría el minuto cero:
- Detección de agentes instalados (`which claude/codex/opencode/...`) y
  mostrar solo los que existen en el launcher.
- Estados vacíos con acción ("No hay carpeta abierta → Abrir carpeta") en vez
  de paneles en blanco.
- Un tour de 3 pasos como overlay descartable (workspace → agente → review).
*Estimado: 2 tandas.*

### 7.3 Presupuestos de rendimiento con regresión automática (P1)
Arreglamos el CPU ocioso midiendo a mano; eso tiene que ser un contrato:
- Budgets explícitos en `tests/runtime`: frame p95 < 8 ms con 20 terminales
  vivos, 0 repaints con ventana sin foco, RSS < 150 MB en idle.
- Bench de `criterion` para las rutas calientes (render de grid, highlight,
  parseo de diff) corriendo en CI con umbral de regresión.
*Estimado: 2 tandas.*

### 7.4 Test de humo end-to-end (P2)
420 tests unitarios y cero tests del app loop entero. `egui_kittest` (harness
oficial de egui) permite: arrancar la app headless, abrir un panel, escribir,
verificar el grid — sin pantalla. Un smoke test así habría cazado varios de
los bugs que encontramos a mano.
*Estimado: 2–3 tandas.*

### 7.5 Diagnóstico exportable (P2, chico)
Ya hay panic log, runs.log y perf HUD. Falta el botón "Exportar diagnóstico"
que junte todo (logs + config + versión + layout anonimizado) en un zip para
adjuntar a un reporte. 1 tanda.

### 7.6 Lo que decidimos NO hacer (igual de importante)
- **Mobile companion** (Orca lo tiene): no — otra plataforma entera, cero
  sinergia con la base Rust/egui actual.
- **SSH worktrees** (Orca lo tiene): no por ahora — duplica cada camino de I/O;
  reevaluar solo si aparece la necesidad real.
- **Telemetría**: no — proyecto personal; el diagnóstico exportable (7.5) cubre
  la necesidad sin recolectar nada.
- **i18n**: no — un solo usuario, un solo idioma. La arquitectura no lo impide
  si algún día hace falta.

### Totales corregidos con esta sección

| | Antes (§6) | Con §7 |
|---|---|---|
| Tandas | 37–48 | **47–60** |
| Tiempo de agente | 22–39 h | **28–48 h** |
| Costo API | 185–360 | **230–440 USD** |
| Calendario | 3–5 semanas | **4–6 semanas** |

---

## 8. Protocolo de ejecución (cómo se trabaja cada ítem)

Autocrítica honesta: la sección 1 está a nivel ejecutable; las secciones 2, 3 y
7 están a nivel de diseño. Eso es deliberado — detallar 60 tandas por
adelantado produce pasos stale (el código cambia debajo del plan). La regla es:

> **Cada tanda arranca escribiendo sus pasos concretos contra el código actual,
> y termina cumpliendo la definición de terminado. El plan fija el qué y el
> porqué; la tanda fija el cómo.**

### Definición de "terminado" (vale para TODO ítem del plan)
1. Lógica nueva en módulo propio con **tests que fijan el comportamiento**
   (incluido el caso borde que motivó el diseño — si Orca lo arregló por un
   bug, nuestro test cita ese caso).
2. `cargo fmt --check` limpio, `clippy --all-targets -- -D warnings` en **0**,
   suite completa verde, `check --release` OK, feature `ghostty-vt` OK.
3. Si toca UI: verificado en pantalla (captura), no solo compilado.
4. Commit con mensaje que explica el porqué; sin código muerto ni `TODO`.
5. El README/atajos actualizados si hay superficie nueva.

### P0 a nivel ejecutable (para arrancar sin pensar)

**P0.1 — Drag & drop de archivos** *(1 tanda)*
- `src/app.rs::begin_frame`: leer `ctx.input(|i| i.raw.dropped_files)` y
  `hovered_files`.
- Hit-test del puntero contra paneles (existe `Workspace::panel_at` /
  `hit_test` en `terminal/panel.rs`); resaltar destino con el ring de foco.
- Nuevo `src/terminal/shell_quote.rs`: `quote_path()` POSIX (comillas simples,
  escape de `'`) + tests con espacios, `'`, unicode, y path con `-` inicial.
- Soltar sobre terminal → `write_all` del path quoteado + espacio. Sobre el
  visor de código → `open_file_viewer(path)`.
- Done: test de shell_quote + captura arrastrando un archivo.

**P0.2 — Screenshot al agente** *(1 tanda)*
- `Command::AttachScreenshot` en paleta (`Ctrl+Shift+S` libre — verificar en
  `shortcuts/mod.rs`).
- `utils/platform.rs`: `capture_interactive() -> Option<PathBuf>` — macOS
  `screencapture -i <tmp>`; Linux `gnome-screenshot -a`/`slurp+grim` si
  existen; Windows: devolver None con toast explicando.
- Con el path: `send_prompt_to_panel(focused, "Mirá esta captura: <path>")`.
- Done: captura llega como path al prompt del agente enfocado; toast confirma.

**P0.3 — Sanitización de session ids** *(1 tanda)*
- `orchestration/agent_sessions.rs`: `sanitize_session_id()` — ≤512 chars, sin
  control chars, rechazar prefijo `-`. Aplicar en `resume_ui` antes de armar
  `claude --resume <id>`.
- Tests: id normal, id `-rf /`, id con `\x1b`, id de 600 chars.

**P0.4 — Worktree add robusto** *(1 tanda)*
- `orchestration/git.rs::create_git_worktree`: timeout de 180 s (matar el
  child al vencer), `--no-track`, y `push.autoSetupRemote=true` solo si
  `git config --get` en todos los scopes da exit 1.
- Tests: los existentes siguen verdes + uno de timeout con un `git` fake que
  duerme.

**P0.5 — Escritura durable del layout** *(1 tanda)*
- Nuevo `src/state/durable_write.rs`: `write_durable(path, bytes)` = tmp →
  `File::sync_all` → rename → `File::open(dir).sync_all()` (best-effort en
  Windows). Ring `.bak.0..4` con espaciado ≥1 h; `load con fallback` slot por
  slot si el principal no parsea.
- Cablear en `state/persistence.rs::save_state_to_path` y `config.rs::save`.
- Tests: round-trip, backup rota con espaciado, restore desde backup con
  principal corrupto, hash no-op (no reescribir si no cambió).

### Regla para P1–P3
Al abrir la tanda de un ítem: (1) escribir en el commit inicial el desglose de
pasos como este, contra el código de ese momento; (2) si el ítem excede su
presupuesto de tandas en +50%, parar y renegociar el alcance en vez de
arrastrarlo. El presupuesto está en §6.

## Apéndice: archivos de Orca consultados

Terminal/persistencia: `src/main/daemon/{headless-emulator,daemon-pty-adapter,daemon-entry,daemon-server,daemon-spawner,history-manager,history-reader,terminal-history-log,session,production-launcher}.ts`, `src/shared/{terminal-serialize-absolute-cursor,terminal-partial-escape-tail}.ts`, `src/main/{terminal-history-gc,terminal-scrollback-snapshots}.ts`.

Agentes: `src/main/agent-hooks/server.ts`, `src/main/claude/{hook-service,hook-settings}.ts`, `src/main/codex/{hook-service,codex-session-link,codex-session-resume-preparation}.ts`, `src/shared/{agent-hook-listener,agent-session-resume,agent-status-types}.ts`, `src/main/native-chat/session-file-resolver.ts`.

Worktrees: `src/main/git/{worktree,worktree-include-file}.ts`, `src/main/{worktree-trash,worktree-removal-safety,worktree-create-base}.ts`, `src/shared/{git-branch-cleanup,git-capability-cache,worktree-base-ref}.ts`.

Review/UX: `src/renderer/src/store/slices/diffComments.ts`, `src/shared/diff-comments-format.ts`, `src/renderer/src/components/diff-comments/useDiffCommentDecorator.tsx`, `src/renderer/src/components/{QuickOpen,WorktreeJumpPalette}.tsx`, `src/renderer/src/components/terminal-pane/use-notification-dispatch.ts`, `src/main/ipc/notifications.ts`.

Persistencia/app: `src/main/{persistence,durable-file-write,active-view-preference,updater}.ts`, `src/main/telemetry/{consent,burst-cap}.ts`, `src/shared/keybindings.ts`, `docs/STYLEGUIDE.md`, `AGENTS.md`.
