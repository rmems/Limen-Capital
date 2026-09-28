# CPU reproducibility baseline — RM-1861

Recorded 2026-09-28 on a fresh checkout of `rmems/Limen-Capital`
(parent commit `f769068`). CPU-only; no GPU, no exchange credentials.

## Toolchains of record

- Julia **1.13.1** (`juliaup` channel `1.13.1+0.x64.linux.gnu`)
- Rust **1.98.1** (`rustc 1.98.1 (48a229cea 2026-09-01)`)

## Qualification matrix

| Check | Command | Result |
|---|---|---|
| Rust fmt | `cd execution && cargo fmt --all -- --check` | **pass** |
| Rust clippy | `cargo clippy --all-targets --all-features --locked -- -D warnings` | **pass** |
| Rust unit tests | `cargo test --locked` | **pass** — 26 passed, 0 failed, 1 ignored |
| Julia brain unit tests (CPU) | `cd brain && LIMEN_RESERVOIR=naut_core julia --project=. test/runtests.jl` | **pass** — all suites green; TemporalFocus optional probe skipped by design |
| Structure gate | `./test_integration.sh` | **pass** — 25 passed, 0 failed |
| Wire smoke | `LIMEN_RESERVOIR=naut_core ./scripts/smoke_wire_local.sh` | **pass** — fixtures verified; corpus-ipc sibling check **skipped** (no `../Limen-Neural` checkout, expected) |
| DendriteTrader suite | `cd DendriteTrader.jl && julia --project=. -e 'using Pkg; Pkg.test()'` @ `07e0d29` | **pass** — all suites green; dYdX v4 integration **skipped** (`DYDX_INTEGRATION` unset) |

Not run (documented skips): GPU EnsembleBrain health check
(`brain/test_brain.jl`, needs NVIDIA GPU), live ZMQ E2E helm, FlatBuffers
proto path (unused in v1). Optional GPU paths preserved — nothing here
removes or gates them.

## Known integration mismatches

- **M-1**: `metabolic-ledger` `GhostWallet` is long-only; DendriteTrader
  supports signed/short paper positions. Integrated fills are long-only spot
  until ledger short support lands (separate issue).
- **M-2**: Triple-Kelly hazard — Julia `SizingModule`, `execution/src/kelly.rs`,
  and ledger adaptive Kelly can stack. Resolved by ADR 0001's one-authority
  rule; enforcement lands with the future dependency-integration issue.
- **M-3**: Julia `Manifest.toml` files are not committed; `[sources]`/`Cargo`
  git+rev pins in `docs/deps.md` are the pins of record.
- **M-4**: `spikenaut-execution-engine` crate name predates the rebrand —
  tracked by Limen-Capital #7, not this issue.
- **M-5**: MarketPulse mixes market features and hardware telemetry
  (`gpu_temp_c`/`gpu_power_w`/`gpu_util_pct`/`basys_buffer_load`, bytes
  84–99); telemetry must not feed market features in research runs
  (ADR 0001).
