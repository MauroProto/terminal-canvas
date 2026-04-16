# Trusted Live Collaboration

## Resumen

`Trusted Live` es la capa de colaboración embebida para compartir el `workspace` activo desde la misma app.

El producto que comparte ya no es un “canvas infinito” como identidad principal. Lo que hoy se comparte es un **desktop acotado de paneles de terminal**, con taskbar, layouts y control remoto por panel.

La sesión siempre corre sobre terminales reales del host. No hay sandbox. Si el host concede control sobre un panel `Controllable`, el invitado puede ejecutar comandos reales dentro de esa terminal.

## Qué soporta hoy

### Host

El host puede:

- iniciar una sesión compartida del workspace activo
- ver y rotar el invite code
- exigir `session passphrase` adicional
- aprobar o rechazar joins pendientes
- marcar dispositivos como confiables
- conceder y revocar control por panel
- cortar la sesión cuando quiera

La UI del host vive dentro de la app:

- pestaña `Online` en la sidebar
- diálogos `Share Workspace` y `Join Shared Session`
- estado de invitados, pedidos de control y terminales controladas

### Invitado

El invitado puede:

- entrar desde la misma app pegando un invite
- abrir la app con `terminalcanvas://join/...` o `--join`
- ver el desktop remoto del workspace compartido
- navegar localmente la vista remota
- pedir control de terminales compartibles
- escribir, pegar y hacer scroll sólo en la terminal que el host le conceda

El invitado no puede:

- abrir carpetas o cambiar de workspace del host
- crear, cerrar o renombrar terminales del host
- mover o redimensionar paneles del host
- tomar control sin aprobación explícita del host

## Modelo de privacidad por panel

Cada panel de terminal tiene un `share_scope` persistido:

- `Private`: no expone texto ni historial al invitado
- `VisibleOnly`: comparte sólo el texto visible actual
- `VisibleAndHistory`: comparte texto visible e historial
- `Controllable`: comparte texto e historial y además permite request/grant de control

La colaboración ya no debe interpretarse como “todo el workspace comparte todo”. El alcance es por panel.

## Arquitectura

### App principal

La app nativa sigue siendo el dueño de:

- workspaces
- paneles y taskbar
- PTYs locales
- render de terminales
- UX de share/join

Archivos principales:

- `src/app.rs`
- `src/terminal/panel.rs`
- `src/state/workspace.rs`

### Módulo de colaboración

`src/collab/` concentra:

- modelos serializables
- protocolo y envelope cifrado
- transporte WebSocket
- snapshots del workspace compartido
- control remoto por terminal
- vista remota para invitados

Archivos principales:

- `src/collab/models.rs`
- `src/collab/protocol.rs`
- `src/collab/transport.rs`
- `src/collab/manager.rs`
- `src/collab/view.rs`

### Servidor embebido del host

La ruta principal es host-direct:

- el host levanta un servidor embebido
- los invitados se conectan directo al host
- el host mantiene la autoridad sobre joins, control y cierre de sesión

Archivo principal:

- `src/collab/server.rs`

Existe además `src/bin/collab-broker.rs` como binario auxiliar para debug y smoke local, no como ruta principal del producto.

## Protocolo y seguridad

### Invite code

El invite code contiene:

- `broker_url`
- `session_id`
- `session_secret`
- `invite_secret`
- `expires_at`
- `tls_cert_pem`
- `requires_passphrase`

Semántica actual:

- `invite_secret` autoriza el join
- `session_secret` protege el payload cifrado de la sesión
- `tls_cert_pem` pinnea el certificado auto-firmado del host
- `expires_at` invalida invites viejos
- `requires_passphrase` obliga a una credencial separada del invite

La separación entre `invite_secret` y `session_secret` es parte del hardening actual. Los joins nuevos ya no deberían degradarse silenciosamente al secreto de sesión.

### Dispositivos confiables

El invitado usa un `device_id` local persistido.

Del lado del host:

- la lista de trusted devices se persiste con el resto del estado de la app
- un join aprobado con `Trust device` puede volver a entrar sin aprobación manual
- el host sigue pudiendo cortar la sesión o revocar acceso

### Payload cifrado

Los mensajes usan:

- `MessagePack`
- `XChaCha20-Poly1305`

El envelope contiene:

- `session_id`
- `sender_id`
- `message_seq`
- `nonce`
- `encrypted_payload`

El receptor debe validar `message_seq` por participante para rechazar duplicados y replays, además de descifrar correctamente el payload.

## Snapshots compartidos

El host publica snapshots construidos desde el estado real del workspace.

`SharedWorkspaceSnapshot` incluye:

- `workspace_id`
- `workspace_name`
- `generated_at`
- `guests`
- `terminal_controls`
- `panels`

`SharedPanelSnapshot` incluye:

- identidad y geometría del panel
- foco/minimizado/alive
- `preview_label`
- `share_scope`
- `visible_text`
- `history_text`
- controller actual y cola de pedidos

La política es:

- paneles privados no exponen texto
- paneles minimizados o detached deben minimizar trabajo y exposición
- el invitado sólo ve y controla lo que el scope del panel permite

## Flujo de control remoto

- el invitado pide control de un panel
- el host aprueba o rechaza
- mientras el control esté activo, el invitado puede enviar input a esa terminal
- si el host interactúa con esa terminal, recupera el control
- al desconectarse un invitado, sus controles se liberan

## Limitaciones actuales

- la sesión sigue siendo por `workspace` activo, no multi-workspace
- el invite code todavía carga bastante poder operativo y debe tratarse como secreto sensible
- el modelo de share scope ya existe, pero la UX de auditoría y revocación todavía es básica
- el transporte sigue usando un loop de polling que conviene endurecer hacia algo más event-driven

## Mapa de código

- `src/app.rs`
- `src/app/dialogs.rs`
- `src/terminal/panel.rs`
- `src/panel.rs`
- `src/state/workspace.rs`
- `src/collab/mod.rs`
- `src/collab/models.rs`
- `src/collab/protocol.rs`
- `src/collab/transport.rs`
- `src/collab/manager.rs`
- `src/collab/server.rs`
- `src/collab/tls.rs`
- `src/collab/view.rs`
- `src/bin/collab-broker.rs`
