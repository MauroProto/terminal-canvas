#!/bin/zsh
set -euo pipefail

ROOT="/Users/mauro/Desktop/proyectos/terminalcanvas"
BIN="$ROOT/target/release/mi-terminal"

cd "$ROOT"

if [ ! -x "$BIN" ]; then
  cargo build --release
fi

GHOSTTY_DYLIB="$(find "$ROOT/target/release/build" -path '*/ghostty-install/lib/libghostty-vt.dylib' -print -quit 2>/dev/null || true)"
if [ -n "$GHOSTTY_DYLIB" ]; then
  export DYLD_LIBRARY_PATH="$(dirname "$GHOSTTY_DYLIB"):${DYLD_LIBRARY_PATH:-}"
  export MI_TERMINAL_BACKEND="${MI_TERMINAL_BACKEND:-ghostty}"
fi

exec "$BIN"
