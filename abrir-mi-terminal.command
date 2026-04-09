#!/bin/zsh
set -euo pipefail

ROOT="/Users/mauro/Desktop/proyectos/terminalcanvas"
BIN="$ROOT/target/release/mi-terminal"

cd "$ROOT"

if [ ! -x "$BIN" ]; then
  cargo build --release
fi

exec "$BIN"
