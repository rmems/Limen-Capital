# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Build & Test

Run from the repository root (each gate uses a subshell so directories do not leak):

```bash
# Rust muscle (fmt + clippy + tests)
(cd execution && cargo fmt --all -- --check \
  && cargo clippy --all-targets --all-features --locked -- -D warnings \
  && cargo test --locked)

# Optional published libraries (Rust 1.99 MSRV)
(cd execution && for feature in kinetic snn corpus-ipc axon-encoder plasticity-lab; do
  cargo build --locked --no-default-features --features "$feature" \
    && cargo test --locked --no-default-features --features "$feature" || exit
done)

# Julia brain (CPU unit suite; no full CUDA instantiate required)
(cd brain && julia --project=. test/runtests.jl)

# Structure / wiring gate
./test_integration.sh
```

## Architecture Overview

Experimental SNN-HFT stack: **Julia brain** (`brain/`) produces binary `ReadoutPacket`
frames; **Rust muscle** (`execution/`) consumes wire frames, applies gates/Kelly, and
ghost-trades via `metabolic-ledger`. Wire contracts live in-tree at
`execution/src/binary_wire.rs`; migration to the optional published corpus-ipc
dependency is deferred. See `docs/deps.md` for release policy and feature boundaries.

## Conventions & Patterns

_Add your project-specific conventions here_
