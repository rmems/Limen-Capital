# ADR 0001: DendriteTrader.jl as the strategy layer

- Status: Accepted
- Date: 2026-09-28
- Driving issue: Linear RM-1861 / GitHub rmems/Limen-Capital#9
- Scope: ownership contract + reproducibility baseline. **No repository
  migration, merge, archive, or history rewrite.** Those require a separate
  explicit decision.

## Context

Limen-Capital is the integrated application: a Julia brain (`brain/`, `math/`)
emits an 88-byte `ReadoutPacket` over ZMQ and the Rust muscle (`execution/`)
consumes it (confidence gate → fractional Kelly → `metabolic-ledger` ghost
fills). DendriteTrader.jl PRs #84–#86 landed deterministic L2 replay, causal
features/labels, and chronological splits — Tasks 1–3 of
`docs/superpowers/plans/2026-09-20-snn-hft-experiments.md`. Task 4 (forecast /
encoder / baseline contracts) proceeds independently inside DendriteTrader.

Today three Kelly implementations can stack silently: DendriteTrader
`SizingModule` (half-Kelly, cap 0.20), `execution/src/kelly.rs` (0.25×
fractional, `b=0.05`), and `metabolic-ledger`'s internal adaptive Kelly on ATP.
This ADR fixes the ownership contract before any dependency integration.

## Ownership contract

| Responsibility | Owner |
|---|---|
| Market replay, causal features, labels, splits | `rmems/DendriteTrader.jl` (`Market`, `Features`, `Experiments`) |
| Forecast models, encoders, baseline policies | `rmems/DendriteTrader.jl` (Task 4 contracts) |
| Sizing **proposal** | `rmems/DendriteTrader.jl` (`SizingModule`) |
| Paper simulation and research execution engine | `rmems/DendriteTrader.jl` (`ExecutionEngine`, `Backtest`) |
| Orchestration, wire adapters (ZMQ/`binary_wire`), systems measurement, dydx feed | `rmems/Limen-Capital` (`brain/`, `execution/`, `wire/`, `strategy/`) |
| Fill application, persistent accounting, realized PnL, JSONL audit | `rmems/metabolic-ledger` (`GhostWallet`, `execute_buy`/`execute_sell`) |
| Transport schemas | `corpus-ipc` once it publishes `MarketPulse`/`ReadoutPacket`; until then in-tree `execution/src/binary_wire.rs` — corpus-ipc is generic transport, never a venue/trading engine |

## Sizing authority — exactly one per run

`forecast → sizing proposal → safety decision → accepted fill → accounting`

- **Research runs** (default): DendriteTrader `SizingModule` is the single
  sizing authority. Limen-Capital `execution/` applies **safety gates only**
  (confidence threshold, max-quantity, ATP bounds) — it must not re-run Kelly
  on a proposal (`use_kelly_sizing = false` when consuming external proposals).
  `metabolic-ledger` records fills accounting-only; its adaptive Kelly state is
  observed for reporting, never applied on top of an upstream proposal.
- **Legacy live-helm runs** (self-contained, no DendriteTrader proposals):
  Rust `kelly.rs` remains the single authority (current default behavior);
  ledger Kelly stays observed-not-applied as above.
- The authority is an explicit per-run selection, never implied by which
  libraries happen to be loaded. Applying two Kelly layers to one signal is a
  bug, not a compounding feature.

## Identifiers, units, clocks

- `run_id`: UTC timestamp + random suffix (`runs/<run_id>/` convention in
  `ExperimentRunner`); `decision_id` and `fill_id` monotonically increasing
  integers per run, carried through the JSONL audit trail.
- Assets: venue-native symbols (`BTC-USD` form). Canonical replay types use
  **integer ticks** for price and integer units for size (DendriteTrader
  `Market` convention); float quantities are allowed only at the proposal
  boundary and quantized before the ledger.
- Costs: **economic fees** (spread/slippage/fee bps, `--cost-bps`) and
  **ATP costs** (`metabolic-ledger` `METABOLIC_COST`, `ENERGY_COMMITMENT`) are
  separate quantities and are never summed into one number.
- Clocks, all nanoseconds epoch: `exchange_ts` (venue event time — the
  ordering key in replay), `receive_ts` (local ingress), `decision_ts`
  (gate output), `event_ts` (fill application). `latency_ns` measures
  decision−observed. Do not mix clocks in ordering keys.
- Market features and hardware telemetry are separate streams by default;
  the current MarketPulse fields at bytes 80–100 (GPU temp/power/util, FPGA
  buffer load) are telemetry and must not feed market features in research
  runs.

## Initial supported instrument/position subset

- Spot paper instruments only; no derivatives on the integrated path.
- Signed (short) paper positions are supported inside DendriteTrader's Julia
  `ExecutionEngine` for research runs.
- `metabolic-ledger` `GhostWallet` is **long-only** today (balances and
  weighted-average cost basis; `execute_sell` cannot create a negative
  position). Integrated fills are therefore **long-only spot** until short
  support lands in `metabolic-ledger` — recorded as integration mismatch
  M-1 below; do not assume or emulate shorts in Rust.

## Pinned revisions and toolchains (baseline of record)

| Component | Revision / version |
|---|---|
| `rmems/DendriteTrader.jl` | `07e0d297f68c8a6edef626d10289e88dae4e9fa5` (main, post-#86) |
| `rmems/metabolic-ledger` | `91822f842c13b0a2b5d8d7b75160933fab2459d6` (Cargo git pin) |
| `rmems/LiquidCortex.jl` | `edb3570ffdbcbc7d631e5a5f990dc82d50b228d7` (`[sources]` pin, optional GPU) |
| `rmems/NeuroPulse.jl` (TemporalFocus) | `ac4aa2ca4c28e63b7a9d2980f5d27348629476b3` (`[sources]` pin, optional) |
| `rmems/kinetic-signals` | `ccc883107e6763969179f036ac33ae22dffdc865` (optional feature `kinetic`) |
| `Limen-Neural/neuromod` | `2a548da6006fedb732b07491b69023476b0cc339` (optional feature `snn`) |
| Julia | 1.13.1 (`[compat] julia = "1.12, 1.13"`; DendriteTrader floor 1.10, CI 1.13) |
| Rust | 1.98.1 (execution `rust-version = "1.98"`, CI pin) |
| Lockfiles | `execution/Cargo.lock` committed; Julia `Manifest.toml` **not** committed — `[sources]` revs above are the pins of record |

## Consequences

- DendriteTrader stays a separately testable Julia package; any future
  Limen-Capital consumption is an additive git+rev `[sources]` pin.
- No recreation of landed replay/features; codec replacement only on a
  demonstrated contract gap.
- Related, not duplicated: Limen-Capital #7 owns the
  `spikenaut-execution-engine` crate rename; Linear RM-1865 owns
  confidence-default documentation.
- Deferred to later issues: DendriteTrader dependency integration, short
  support in `metabolic-ledger`, repository migration/archive, corpus-ipc
  wire schema adoption.
