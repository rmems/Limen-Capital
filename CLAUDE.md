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
frames; **Rust muscle** (`execution/`) consumes wire frames and gates/logs a
fractional-Kelly policy quantity. The pinned `metabolic-ledger` helpers accept no
quantity and size actual ghost trades themselves using adaptive Kelly.
ADR 0001's single-sizing-authority rule is a target, not implemented behavior.
Both `research_helm.jl` / `ExperimentRunner` and `spike_helm.jl` require CUDA,
even with `LIMEN_RESERVOIR=naut_core`. The brain CPU unit suite builds no reservoir;
the separate root CPU comparison explicitly uses `reference_cpu`.
Wire contracts live in-tree at
`execution/src/binary_wire.rs`; migration to the optional published corpus-ipc
dependency is deferred. See `docs/deps.md` for release policy and feature boundaries.

## Conventions & Patterns

- Keep documentation explicit about synthetic experiments versus held-out market evidence.
- Track work in GitHub/Linear; no beads setup is required.
- Preserve user changes and stashes. Stashes are shared across worktrees; do not clear them.
