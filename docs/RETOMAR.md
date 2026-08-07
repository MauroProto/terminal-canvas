# Dónde retomamos (P3.15 T3)

Estado: **todo compila, clippy limpio y 645 tests + suites de integración
verdes**, con y sin el feature `daemon`. El árbol está limpio y committeado
(`ee876b9`).

## Lo que falta para cerrar T3

La plomería está hecha y testeada, pero **los terminales de la app todavía se
crean in-process**. Falta un solo paso, y es de ordenamiento:

`TerminalApp::build()` crea los workspaces (y con ellos el primer terminal)
**antes** de adoptar el daemon, así que cuando se instala el spawner los PTYs
ya existen localmente. Verificado: la app arranca, adopta el daemon, pero
`list` en el daemon devuelve 0 sesiones.

Para cerrarlo:

1. En `build()`, adoptar el daemon **al principio** (hoy se hace dentro del
   literal de `Self`, al final) y guardarse el `endpoint`.
2. Justo después de crear cada `Workspace` — antes de `spawn_terminal` en la
   rama nueva, y antes del primer `ensure_attached` en la rama restaurada —
   instalar el spawner en su `PtyManager` con `set_remote_spawner`.
3. Enganchar también `attach_detached` al spawner, pasándole el `session_id`
   que ya tiene la sesión detached como `desired_id` (para eso se agregó el id
   propuesto por el cliente al protocolo). Sin esto, los paneles restaurados
   se atachean in-process.
4. Reattach al arrancar: si el daemon ya tiene sesiones de una corrida
   anterior, usar `sessions::attach_existing` en vez de crear nuevas. Ya está
   escrito y sin usar.

## Cómo verificar que quedó cerrado

```sh
cargo build --features daemon --bins
rm -rf "$HOME/Library/Application Support/terminal-app/daemon"
RUST_LOG=info ./target/debug/mi-terminal &
sleep 10
# el daemon tiene que reportar >= 1 sesión
python3 - <<'PY'
import socket, json, os
d=os.path.expanduser("~/Library/Application Support/terminal-app/daemon")
tok=open(os.path.join(d,"token")).read().strip()
s=socket.socket(socket.AF_UNIX); s.connect(os.path.join(d,"daemon-v1.sock"))
f=s.makefile("rw")
send=lambda m:(f.write(json.dumps(m)+"\n"), f.flush())
def resp():
    while True:
        m=json.loads(f.readline())
        if m["type"] in ("output","exit"): continue
        return m
send({"type":"hello","version":1,"token":tok}); resp()
send({"type":"list"}); print("sesiones:", len(resp()["ids"]))
PY
```

Y la prueba real: matar la app, volver a abrirla, y que el terminal siga con
su historial y su proceso vivo.

## La otra salvedad (no bloqueante)

P2.11: `TerminalPanel` sigue con `session` (hoja raíz) + `leaf_sessions` en vez
del `HashMap<LeafId, SessionController>` uniforme de T2. Es una diferencia de
layout de datos sin efecto observable; el scroll, el input, el render y la
persistencia ya son por hoja.
