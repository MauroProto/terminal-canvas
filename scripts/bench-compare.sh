#!/usr/bin/env bash
# Compara los benches de las rutas calientes contra el commit anterior y falla
# si alguno se degradó más del umbral (Ship-it 7.3, T2).
#
# Uso: scripts/bench-compare.sh [base-ref] [umbral-porcentual]
#   scripts/bench-compare.sh            # contra HEAD~1, umbral 15%
#   scripts/bench-compare.sh origin/master 10
set -euo pipefail

# Los porcentajes de criterion se parsean con punto decimal: sin esto, un
# locale con coma haría que "+22.4%" se lea como 22 (o peor).
export LC_ALL=C

BASE_REF="${1:-HEAD~1}"
THRESHOLD="${2:-15}"
BENCH_ARGS=(--warm-up-time 1 --measurement-time 3)

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Baselines compartidas entre el worktree base y el actual.
CRITERION_DIR="$(mktemp -d)"
WORKTREE_DIR="$(mktemp -d)"
cleanup() {
  git worktree remove --force "$WORKTREE_DIR" >/dev/null 2>&1 || true
  rm -rf "$CRITERION_DIR" "$WORKTREE_DIR"
}
trap cleanup EXIT

export CRITERION_HOME="$CRITERION_DIR"

echo "== Baseline: $BASE_REF =="
git worktree add --detach "$WORKTREE_DIR" "$BASE_REF" >/dev/null
if [[ ! -f "$WORKTREE_DIR/benches/hot_paths.rs" ]]; then
  # El commit base es anterior a los benches: no hay con qué comparar.
  echo "El base ($BASE_REF) no tiene benches/hot_paths.rs: nada que comparar."
  exit 0
fi
(
  cd "$WORKTREE_DIR"
  cargo bench --bench hot_paths -- "${BENCH_ARGS[@]}" --save-baseline base >/dev/null
)

echo "== Actual: $(git rev-parse --short HEAD) =="
OUTPUT="$(cargo bench --bench hot_paths -- "${BENCH_ARGS[@]}" --baseline base 2>&1)"
echo "$OUTPUT"

# Criterion imprime, por bench, una línea "change: [... +12.3% ...]".
# Nos quedamos con el punto medio (segundo porcentaje) de cada una.
REGRESSIONS="$(
  echo "$OUTPUT" | awk -v threshold="$THRESHOLD" '
    /^[A-Za-z_0-9]+ *$/ { name = $1 }
    /change:/ {
      # change: [-1.2345% +0.1234% +1.5678%] (p = 0.00 < 0.05)
      if (match($0, /\[[^]]*\]/)) {
        inner = substr($0, RSTART + 1, RLENGTH - 2)
        n = split(inner, parts, / +/)
        mid = parts[2]
        gsub(/%/, "", mid)
        if (mid + 0 > threshold + 0) {
          printf "%s regresó %.2f%% (umbral %s%%)\n", name, mid, threshold
        }
      }
    }
  '
)"

if [[ -n "$REGRESSIONS" ]]; then
  echo
  echo "REGRESIONES DE PERFORMANCE:"
  echo "$REGRESSIONS"
  exit 1
fi

echo
echo "OK: ningún bench se degradó más de ${THRESHOLD}%."
