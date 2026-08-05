#!/bin/zsh
# Watchdog de mi-terminal: mantiene la app viva ante cierres anormales.
#
# - Salida limpia (código 0: cerrar la ventana, Cmd+Q) => NO se relanza.
# - Cualquier otra salida (panic tras 5 frames rotos => exit 1, SIGKILL,
#   SIGTERM, OOM del sistema) => se relanza en ~1s; el estado se restaura
#   solo gracias al autosave periódico de la app.
# - Anti-crashloop: 5 reinicios en menos de 60s => el watchdog se rinde y
#   lo deja registrado, para no quemar CPU si algo está roto de raíz.
#
# Uso: nohup scripts/mi-terminal-watchdog.sh >/dev/null 2>&1 & disown
# Log del watchdog: /tmp/mi-terminal-watchdog.log
# Log de la app:    /tmp/mi-terminal-run.log

set -u
cd "$(dirname "$0")/.."

BIN="${MI_TERMINAL_BIN:-./target/debug/mi-terminal}"
APP_LOG=/tmp/mi-terminal-run.log
WATCHDOG_LOG=/tmp/mi-terminal-watchdog.log

note() {
    echo "$(date '+%F %T') $1" >>"$WATCHDOG_LOG"
}

note "watchdog iniciado (bin: $BIN)"
restarts=()
while true; do
    RUST_LOG=info "$BIN" >>"$APP_LOG" 2>&1
    code=$?
    if [ "$code" -eq 0 ]; then
        note "salida limpia (código 0); no se relanza"
        exit 0
    fi

    now=$(date +%s)
    restarts+=("$now")
    recent=()
    for t in "${restarts[@]}"; do
        if [ $((now - t)) -lt 60 ]; then
            recent+=("$t")
        fi
    done
    restarts=("${recent[@]}")
    if [ "${#restarts[@]}" -ge 5 ]; then
        note "5 reinicios en <60s (último código: $code); watchdog detenido"
        exit 1
    fi

    note "mi-terminal terminó con código $code; relanzando en 1s"
    sleep 1
done
