#!/usr/bin/env bash
# Local (non-CI) smoke: pack/decode fixtures + cargo/julia tests. No live ZMQ required.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIMEN_NEURAL="${LIMEN_NEURAL:-$(cd "$ROOT/../Limen-Neural" 2>/dev/null && pwd || true)}"
echo "ROOT=$ROOT"
echo "LIMEN_NEURAL=${LIMEN_NEURAL:-<unset>}"
echo "== corpus-ipc =="
if [ -n "$LIMEN_NEURAL" ] && [ -d "$LIMEN_NEURAL/corpus-ipc" ]; then
  (cd "$LIMEN_NEURAL/corpus-ipc" && cargo test --quiet)
else
  echo "skip corpus-ipc (set LIMEN_NEURAL)"
fi
echo "== Capital execution =="
(cd "$ROOT/execution" && cargo test --quiet)
echo "== Capital julia wire tests =="
(cd "$ROOT/brain" && julia --project=. test/runtests.jl)
echo "== fixtures =="
test -f "$ROOT/wire/fixtures/marketpulse.bin"
test -f "$ROOT/wire/fixtures/readout.bin"
echo "OK — NervousWire local verification passed"
echo ""
echo "Manual E2E (3 terminals):"
echo "  1. cd execution && cargo run --bin market_sim"
echo "  2. cd brain && julia --project=. spike_helm.jl   # needs GPU + full brain"
echo "  3. cd execution && cargo run --release            # binary SUB :5556"
echo "Or muscle-only with a pre-baked readout pub (see wire/README.md)."
