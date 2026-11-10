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
| Rust clippy | `cd execution && cargo clippy --all-targets --all-features --locked -- -D warnings` | **pass** |
| Rust unit tests | `cd execution && cargo test --locked` | **pass** — 26 passed, 0 failed, 1 ignored |
| Julia brain unit tests (CPU) | `cd brain && julia --project=. test/runtests.jl` | **pass** (historical record) — local CPU includes only; no reservoir is loaded |
| Structure gate | `./test_integration.sh` | **pass** — 25 passed, 0 failed |
| Wire smoke | `./scripts/smoke_wire_local.sh` | **pass** (historical record) — fixtures verified; corpus-ipc sibling check **skipped** (no `../Limen-Neural` checkout, expected); no reservoir is loaded |
| DendriteTrader suite | `git clone https://github.com/rmems/DendriteTrader.jl && cd DendriteTrader.jl && git checkout 07e0d29 && julia --project=. -e 'using Pkg; Pkg.test()'` | **pass** — all suites green; dYdX v4 integration **skipped** (`DYDX_INTEGRATION` unset) |

Clarification (RM-2447): the original unit/smoke commands set
`LIMEN_RESERVOIR=naut_core`, but those checks never build a reservoir, so the
setting provided no CPU-backend qualification. The brain CPU suite does not
exercise TemporalFocus. CI's separate root CPU comparison explicitly selects
`reference_cpu`; it is not a CPU mode for either helm.

Not run (documented skips): GPU EnsembleBrain health check
(`brain/test_brain.jl`, needs NVIDIA GPU), live ZMQ E2E helm, FlatBuffers
proto path (unused in v1). Both `research_helm.jl` / `ExperimentRunner` and
`spike_helm.jl` require functional CUDA, including with `naut_core`; these CPU
checks do not qualify them.

## Known integration mismatches

- **M-1**: `metabolic-ledger` `GhostWallet` is long-only; DendriteTrader
  supports signed/short paper positions. Integrated fills are long-only spot
  until ledger short support lands (separate issue).
- **M-2 (open)**: ADR 0001 defines, but does not enforce, one sizing authority.
  Rust `execution/src/kelly.rs` gates/logs a policy quantity; the pinned ledger's
  `execute_buy` / `execute_sell` accept no quantity and apply their own adaptive
  Kelly sizing. Disabling Rust Kelly does not make ledger calls accounting-only.
  Exact upstream-sized fills remain unimplemented; future DendriteTrader
  `SizingModule` integration must not introduce another sizing layer.
- **M-3**: Julia `Manifest.toml` files are not committed; `[sources]`/`Cargo`
  git+rev pins in `docs/deps.md` are the pins of record.
- **M-4**: `spikenaut-execution-engine` crate name predates the rebrand —
  resolved: the crate is now `limen-execution` (Limen-Capital #7).
- **M-5**: MarketPulse mixes market features and hardware telemetry
  (`gpu_temp_c`/`gpu_power_w`/`gpu_util_pct`/`basys_buffer_load`, bytes
  84–99); telemetry must not feed market features in research runs
  (ADR 0001).
- **M-6**: Current integrated path violates that separation —
  `reservoir_step!` feeds `gpu_temp_c`/`basys_buffer_load` to
  `hft_inhibition` (`brain/experiment_runner.jl:104`,
  `brain/spike_helm.jl:254-256`), so telemetry affects decisions today.
  Fix deferred to the integration issue.
- **M-7**: Audit-ID contract not yet implemented — the `run_id`
  assignment (`brain/metrics.jl:259-260`) uses local `Dates.now()` +
  `-s<seed>` with no random suffix (same-second collisions share an
  artifact dir), and
  `decision_id`/`fill_id` are not carried through execution/ledger paths.
  Required by ADR 0001; enforcement lands with the integration issue.
