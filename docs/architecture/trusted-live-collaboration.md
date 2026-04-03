# Trusted Live Collaboration

## Resumen

`Trusted Live` es la primera versión de colaboración remota embebida en la app para compartir un `workspace` local con otras personas de confianza.

Objetivo actual:

- compartir un único `workspace` activo
- usar terminales reales del host
- permitir hasta `4` participantes totales contando al host
- usar `request control` por terminal
- mantener al host como autoridad total de la sesión

No es un sandbox. Un invitado aprobado con control sobre una terminal puede ejecutar comandos reales en esa sesión del host.

## Qué quedó implementado

### Host

El host puede:

- iniciar una sesión compartida del `workspace` actual
- ver un `invite code`
- ver cuándo vence el invite actual
- rotar el invite sin cortar la sesión ya viva
- configurar una `session passphrase` opcional que no viaja dentro del invite
- aprobar o rechazar pedidos de entrada
- marcar un join como `Trust device` para no tener que aprobar siempre el mismo dispositivo
- aprobar pedidos de control por terminal
- revocar el control de una terminal en cualquier momento
- cerrar la sesión compartida

La UI del host quedó integrada en la app, sin meter nada en la sidebar:

- HUD superior derecho con `Share`, `Join`, `Session`, `Stop`, `Leave`
- modal `Share Workspace`
- panel de sesión con:
  - estado
  - invite code
  - pending joins
  - control requests
  - guests conectados
  - terminales actualmente controladas

## Invitado

El invitado puede:

- entrar desde la misma app pegando un `invite code`
- entrar también lanzando la app con un invite code como argumento o con `--join <invite>`
- ingresar una `session passphrase` si el host configuró una
- reusar el mismo dispositivo para volver a entrar sin aprobación manual si el host ya lo marcó como confiable
- ver el canvas remoto del workspace compartido
- panear, zoomear y enfocar paneles localmente
- pedir control de una terminal
- escribir, pegar y hacer scroll sólo en la terminal que le fue otorgada
- liberar manualmente el control de una terminal

El invitado no puede:

- abrir carpetas
- crear o cerrar terminales
- mover o redimensionar paneles del host
- renombrar paneles
- ver otros workspaces del host desde la UI
- ejecutar comandos locales de la app como `Open Folder`, `New Terminal` o `Launch Agent`

## Reglas de control por terminal

- cada terminal tiene un solo `controller` a la vez
- puede haber varias terminales controladas en paralelo por distintos invitados
- cualquier invitado puede pedir control de cualquier terminal
- el host siempre puede revocar control
- si el host escribe o scrollea sobre una terminal controlada, recupera el control de inmediato
- si un invitado se desconecta, sus controles se liberan

## Arquitectura

La arquitectura quedó separada en tres piezas:

### 1. App desktop

La app principal sigue siendo `eframe/egui` y mantiene:

- layout del canvas
- workspaces
- PTYs locales
- render de terminales
- UX de share/join

Archivos principales:

- [src/app.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/app.rs)
- [src/terminal/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/panel.rs)
- [src/state/workspace.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/state/workspace.rs)

### 2. Módulo de colaboración

Se agregó un módulo nuevo `src/collab/`:

- [src/collab/models.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/models.rs)
- [src/collab/protocol.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/protocol.rs)
- [src/collab/transport.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/transport.rs)
- [src/collab/manager.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/manager.rs)
- [src/collab/view.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/view.rs)

Responsabilidades:

- creación y join de sesiones
- manejo de `invite code`
- cifrado del payload app-level
- transporte WebSocket en background
- snapshots del workspace compartido
- request/grant/revoke de control
- vista remota del canvas para invitados
- reconexión automática básica del transporte cuando el WebSocket cae inesperadamente
- servidor embebido en la máquina del host para compartir la sesión sin relay externo
- expiración y rotación de invites sin invalidar la sesión ya conectada
- auto-aprobación por dispositivo confiable persistida del lado del host

### 3. Servidor directo del host

La ruta principal ahora es que la app del host levante el servidor de la sesión compartida:

- [src/collab/server.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/server.rs)

Ese servidor:

- crea sesiones compartidas
- recibe joins
- aprueba/deniega invitados
- mantiene conexiones WebSocket
- hace relay del tráfico entre host e invitados

Además quedó un binario separado para debug o smoke local:

- [src/bin/collab-broker.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/bin/collab-broker.rs)

No es la ruta principal del producto.

## Modelo de red

Topología actual:

- el host abre un servidor embebido dentro de la app
- los invitados se conectan directo a la máquina del host
- se usa HTTPS para crear/join/approve/deny contra el host
- se usa HTTPS también para terminar una sesión explícitamente desde el host
- se usa WebSocket seguro (`WSS`) para streaming bidireccional
- el transporte del cliente queda vivo aunque se cierre una sesión y puede volver a conectarse sin reiniciar la app
- no se sube el workspace ni las terminales a un relay público
- para acceso desde otra casa, la URL alcanzable del host sigue dependiendo de la red del host, su router y su reachability real

### Endpoints implementados

- `POST /v1/share-sessions`
- `POST /v1/share-sessions/{id}/join`
- `POST /v1/share-sessions/{id}/end`
- `POST /v1/share-sessions/{id}/approve`
- `POST /v1/share-sessions/{id}/deny`
- `POST /v1/share-sessions/{id}/rotate-invite`
- `GET /v1/share-sessions/{id}/stream` vía WebSocket

## Protocolo y cifrado

### Invite code

El `invite code` contiene:

- `broker_url`
- `session_id`
- `session_secret`
- `invite_secret`
- `expires_at`
- `tls_cert_pem`
- `requires_passphrase`

En el modo host-direct actual:

- `broker_url` es en realidad la URL alcanzable del host
- `session_secret` no es sólo metadata del cliente: el servidor embebido del host lo registra al crear la sesión
- `session_secret` quedó reservado al cifrado end-to-end del payload
- el `join` ahora valida un `invite_secret` separado, así que el host puede rotar invites sin romper la sesión activa
- `expires_at` permite invalidar invites viejos automáticamente
- esto evita que conocer sólo la URL y el `session_id` alcance para entrar a una sesión abierta
- `tls_cert_pem` pinnea el certificado auto-firmado del host dentro del invite, para que el invitado valide exactamente ese host y no un tercero
- `requires_passphrase` le avisa al cliente que además de invite necesita una passphrase separada

### Trust por dispositivo

El invitado ahora entra con un `device_id` local persistido en su instalación.

Del lado del host:

- el estado de dispositivos confiables se persiste junto al resto del estado liviano de la app
- al aprobar con `Trust device`, ese `device_id` queda recordado
- las sesiones futuras arrancan con esa lista y el servidor embebido auto-aprueba ese dispositivo

Esto no crea cuentas ni usuarios formales. Es simplemente una memoria de confianza por instalación/dispositivo.

Se serializa en JSON y se codifica con base64 URL-safe usando el prefijo:

- `terminalcanvas://join/`

Implementación:

- [src/collab/protocol.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/protocol.rs)

### Payload cifrado

Los mensajes de colaboración usan:

- `MessagePack` para serialización
- `XChaCha20-Poly1305` para cifrado del payload

El envelope contiene:

- `session_id`
- `sender_id`
- `message_seq`
- `nonce`
- `encrypted_payload`

El broker no interpreta el contenido cifrado de snapshots ni de input remoto.

## Snapshots del workspace

El host publica snapshots del `workspace` compartido construidos desde el estado real del canvas.

Cada `SharedWorkspaceSnapshot` incluye:

- `workspace_id`
- `workspace_name`
- `generated_at`
- `guests`
- `terminal_controls`
- `panels`

Cada `SharedPanelSnapshot` incluye:

- `panel_id`
- `title`
- `position`
- `size`
- `color`
- `z_index`
- `focused`
- `alive`
- `preview_label`
- `visible_text`
- `history_text`
- `controller`
- `controller_name`
- `queue_len`

La extracción del snapshot de terminal se hace desde el PTY local:

- [src/terminal/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/panel.rs)

### Dedupe de snapshots

El host ya no reenvía snapshots en cada frame sólo porque cambió `generated_at`.

La comparación actual ignora ese timestamp y sólo publica cambios cuando se modifica algo relevante:

- guests
- controles por terminal
- paneles
- nombre/id del workspace

## Input remoto

Se agregó un tipo de input remoto reutilizable:

- `TerminalInputEvent`

Eventos soportados:

- `Text`
- `Paste`
- `Key`
- `Scroll`

El invitado no escribe directo sobre el PTY. Manda `GuestTerminalInput` al host, y el host lo aplica sobre la terminal real sólo si ese guest es el controlador actual.

Integración:

- [src/terminal/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/panel.rs)
- [src/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/panel.rs)
- [src/state/workspace.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/state/workspace.rs)
- [src/app.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/app.rs)

## UX actual

### Comandos

Se agregaron dos acciones nuevas:

- `Share Workspace`
- `Join Shared Session`

Disponibles en:

- command palette
- shortcuts

Archivos:

- [src/command_palette/commands.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/command_palette/commands.rs)
- [src/shortcuts/mod.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/shortcuts/mod.rs)
- [src/shortcuts/default_bindings.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/shortcuts/default_bindings.rs)

Shortcuts:

- `Ctrl+Shift+S` → `Share Workspace`
- `Ctrl+Shift+J` → `Join Shared Session`

Arranque directo con invite:

- `cargo run --bin mi-terminal -- 'terminalcanvas://join/...'`
- `cargo run --bin mi-terminal -- --join 'terminalcanvas://join/...'`
- o vía env `TERMINAL_CANVAS_JOIN_INVITE`

### HUD

La colaboración no se metió en la sidebar. Se resolvió con un HUD arriba a la derecha:

- estado de sesión
- acciones rápidas según modo

### Canvas invitado

El invitado ve un `remote mirror` del workspace compartido. No renderiza PTYs locales: renderiza snapshots remotos.

Implementación:

- [src/collab/view.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/view.rs)

## Seguridad actual

### Sí contempla

- warning explícito de que `Trusted Live` no es sandbox
- aprobación manual del host para el join
- aprobación manual del host para control por terminal
- revocación inmediata
- payload app-level cifrado
- transporte HTTPS/WSS directo al host con certificado pinneado por invite
- `session passphrase` opcional adicional al invite code, hasheada con `Argon2id`
- throttling/backoff de joins fallidos para complicar brute force de la passphrase
- sesión cerrada al cerrar la app del host
- los invitados no pueden disparar diálogos locales ni manipular workspaces desde la UI

### No contempla

- aislamiento fuerte por carpeta
- sandbox
- permisos granulares por comando
- autenticación de cuentas
- persistencia o reanudación de sesiones compartidas

## Limitaciones conocidas

- el join ya puede entrar por argumento/env, pero todavía no hay registro nativo del deep link en el sistema operativo
- el acceso desde otra red sigue necesitando que la URL del host sea realmente alcanzable desde internet
- el binario `collab-broker` quedó sólo como herramienta de debug/smoke y no es la ruta principal del producto
- la copia/selección de texto remoto todavía es básica
- no hay cliente web
- no hay sincronización de clipboard del host
- el control del canvas compartido sigue siendo del host; el invitado sólo navega su propia cámara
- el workspace compartido está limitado a uno por sesión

## Decisiones importantes

- `Trusted Live` sigue siendo acceso a terminales reales del host, no sandbox.
- La restricción a un solo workspace compartido es una decisión de producto/UI, no una garantía de aislamiento del shell.
- El host sigue siendo la única autoridad sobre membresía, grants/revokes, layout compartido e input real al PTY.
- Cada invitado mantiene su propia cámara local; el viewport no se sincroniza globalmente.
- El transporte directo ya no depende de `http/ws`: quedó en `https/wss` con certificado auto-firmado generado por el host y pinneado en el invite code.
- La passphrase de sesión no se guarda en claro: el host almacena sólo un hash `Argon2id` y el server directo verifica contra eso.
- El transporte WebSocket del cliente quedó en modo non-blocking para no congelar `Close`, pings ni reconexiones cuando la conexión está idle.
- Cerrar una sesión compartida ya no mata el thread del transporte; la app puede volver a compartir o volver a unirse sin recrear el proceso.
- Si el host pierde el socket por un corte corto, el servidor directo conserva una ventana breve para reconectar antes de terminar la sesión.
- Si una conexión vieja se cae después de que ya existe una nueva, el servidor directo ignora ese disconnect viejo y no pisa la conexión actual.

## Cómo levantarlo

### App host / invitado

```bash
cargo run --bin mi-terminal
```

Variables opcionales:

- `TERMINAL_CANVAS_SHARE_URL`
- `TERMINAL_CANVAS_JOIN_INVITE`

### Broker de debug opcional

```bash
cargo run --bin collab-broker
```

No es necesario para el flujo principal host-direct. Quedó sólo como utilitario de debug/smoke local.

## Flujo de uso actual

### Host

1. Abrir la app.
2. Ir al workspace que querés compartir.
3. Ejecutar `Share Workspace`.
4. Configurar opcionalmente una `session passphrase`.
5. Confirmar el warning de `Trusted Live`.
6. Copiar el invite code.
7. Compartir la passphrase por un canal separado si la usaste.
8. Aprobar joins.
9. Aprobar control requests por terminal.

### Invitado

1. Abrir la app.
2. Ejecutar `Join Shared Session`.
3. Pegar invite code.
4. Ingresar `display name`.
5. Ingresar la `session passphrase` si el host la configuró.
6. Esperar aprobación.
7. Pedir control de una terminal.

## Archivos tocados por la funcionalidad

### Desktop app

- [src/main.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/main.rs)
- [src/app.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/app.rs)
- [src/command_palette/commands.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/command_palette/commands.rs)
- [src/shortcuts/mod.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/shortcuts/mod.rs)
- [src/shortcuts/default_bindings.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/shortcuts/default_bindings.rs)
- [src/terminal/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/terminal/panel.rs)
- [src/panel.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/panel.rs)
- [src/state/workspace.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/state/workspace.rs)

### Collaboration layer

- [src/collab/mod.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/mod.rs)
- [src/collab/models.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/models.rs)
- [src/collab/protocol.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/protocol.rs)
- [src/collab/transport.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/transport.rs)
- [src/collab/manager.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/manager.rs)
- [src/collab/server.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/server.rs)
- [src/collab/tls.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/tls.rs)
- [src/collab/view.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/collab/view.rs)

### Broker

- [src/bin/collab-broker.rs](/Users/mauro/Desktop/proyectos/terminalcanvas/src/bin/collab-broker.rs)

### Dependencias

- [Cargo.toml](/Users/mauro/Desktop/proyectos/terminalcanvas/Cargo.toml)
- [Cargo.lock](/Users/mauro/Desktop/proyectos/terminalcanvas/Cargo.lock)

## Estado actual

Estado real hoy:

- compila
- tests completos pasando
- app levantando
- servidor embebido del host probado por test
- la sesión compartida puede arrancar directo desde la app del host sin relay externo
- la conexión host/invitado directa ya usa HTTPS/WSS con certificado pinneado en el invite
- la sesión puede requerir una passphrase extra y el server directo hace backoff sobre intentos fallidos
- transporte con reconexión básica y thread reutilizable probado por tests
- servidor directo con cierre explícito de sesión, gracia de reconexión del host y cleanup periódico
- guest no entra en `Live` hasta recibir aprobación real
- snapshots compartidos ya no se republcan por un timestamp irrelevante

Todavía falta una pasada dedicada de QA multi-instancia real para cerrar la UX host/invitado de punta a punta.
