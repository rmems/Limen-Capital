#!/usr/bin/env bash
# Required executable boundary test; no GPU, sockets, or optional Julia packages.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf -- "$TMP"' EXIT
# Never inherit a caller's alternative symbol table into canonical tests.
export LIMEN_UNIVERSE="$ROOT/config/universe.toml"
python3 "$ROOT/wire/generate_fixtures.py"
julia --startup-file=no --project="$ROOT/brain" "$ROOT/brain/test/emit_wire_frames.jl" "$TMP"
export LIMEN_JULIA_WIRE_DIR="$TMP"
cargo test --manifest-path "$ROOT/execution/Cargo.toml" --locked --test julia_wire -- --ignored --nocapture
