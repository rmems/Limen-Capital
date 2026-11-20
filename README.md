# Limen-Capital — Experimental SNN-HFT (Julia Brain + Rust Muscle)

Hybrid research stack: **Julia neuromorphic strategy** + **Rust deterministic execution**.
The Julia/wire/ghost-ledger path is for experimental spiking / liquid-state
market research, **not live capital**. The separate, feature-gated `limen-live`
experiment targets real-money micro-capital on **Alpaca (US stocks and ETFs)**.
It is not a production trading system.

**Live launch is blocked on this revision:** `limen-live` still has the legacy
Robinhood adapter and refuses `paper = false`; there is no Alpaca adapter or
live unlock. The Claude decider is wired for paper runs behind enforced model
profiles and a persistent LLM budget, but it cannot submit live orders.
Do not provision real keys or treat `config/live-soak.draft.toml` as runnable.
See the [live runbook and release gates](docs/live-runbook.md) and
[security model](docs/SECURITY.md). There is no paper phase or shadow soak:
once the release gates are met, the first supervised real-money session is the
soak, at micro caps ($10 position / at most $5 order / $4 daily loss, preferably
$1 orders). CI/branch/secret protection [#26](https://github.com/rmems/Limen-Capital/issues/26)
is a release blocker.

A `limen-live` journal is an operational audit, **not a research result**.
The journal and LLM output never enter a training set or research metrics.

This repo is a **standalone application** (your personal/org remote). It is **not**
published under the Limen-Neural GitHub organization. It *depends on* validated
crates.io releases and git+rev library pins (see `docs/deps.md`);
in-tree `naut_core` works without them.

## Dependencies

Validated Rust packages use **crates.io releases** with `Cargo.lock`, except
`metabolic-ledger`, which retains its **`git` + `rev`** pin. Julia packages retain
`[sources]` pins. No sibling `Limen-Neural/` checkout is required.

| Package | Role |
|---------|------|
| **metabolic-ledger** | Ghost wallet, ATP gates, JSONL (git pin) |
| **LiquidCortex.jl** | Preferred CUDA LSM; fallback `naut_core` also requires CUDA in the helms |
| **TemporalFocus** (NeuroPulse.jl) | Pinned dependency, not wired into the helms; `adapters.jl` has a load probe |
| **binary_wire** (in-tree) | MarketPulse 120B / ReadoutPacket 88B; corpus-ipc migration deferred |
| **kinetic-signals / neuromod** | Optional published libraries (`kinetic` / `snn`) |
| **corpus-ipc / axon-encoder / plasticity-lab** | Optional published libraries (same-named features); runtime wiring deferred |

See **[docs/deps.md](docs/deps.md)**, **[docs/wire-protocol-v1.md](docs/wire-protocol-v1.md)**,
**[docs/SECURITY.md](docs/SECURITY.md)**, the ownership contract in
**[docs/adr/0001-strategy-layer-ownership.md](docs/adr/0001-strategy-layer-ownership.md)**,
and the CPU baseline in **[docs/repro-baseline.md](docs/repro-baseline.md)**.

For the small CPU-only reservoir versus shared forecast baselines campaign, see
**[docs/cpu-comparison.md](docs/cpu-comparison.md)**. It uses a fixed one-lot paper
policy, not the live helm's confidence/Kelly path. Research confidence defaults and
RM-1865 calibration boundaries: **[docs/confidence-defaults.md](docs/confidence-defaults.md)** (GitHub #5).

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Julia Brain                                            │
│  MarketEncoder (28 spikes)                              │
│       ↓                                                 │
│  LiquidCortex EnsembleBrain (4 lobes × 65K)             │
│       ↓                                                 │
│  In-tree causal NERO relevance                          │
│       ↓                                                 │
│  16-element readout + 4 relevance scores                │
└──────────────────────┬──────────────────────────────────┘
                       │ 88-byte binary over ZMQ TCP (v1)
                       ↓
┌──────────────────────────────────────────────────────────┐
│  Rust Muscle                                             │
│  Confidence / Kelly policy gates → caller-sized ghost fills    │
│  latency tracking → JSONL; dydx adapter unwired          │
└──────────────────────────────────────────────────────────┘
```

## Directory structure

```text
Limen-Capital/
├── brain/           # Julia brain (helm, reservoir, metrics, tests)
├── math/            # Hawkes / fractal / SDE / FastReflex (FeatureStream uses Hawkes)
├── execution/       # Rust muscle (wire, Kelly Decision, metabolic-ledger)
├── wire/            # NervousWire v1 docs + golden fixtures
├── docs/            # deps + wire protocol
├── scripts/         # local smoke (not CI)
├── strategy/        # JSON adapter (secondary)
└── proto/           # FlatBuffers (optional / unused v1)
```

## Quick start

### Prerequisites

- Julia **1.13** (Pkg `[sources]` pins; CI uses latest stable 1.13), Rust **1.99+** (CI pins 1.99.0), network for first-time dependencies
- NVIDIA CUDA-capable GPU required for `test_brain.jl`, `research_helm.jl`
  (`ExperimentRunner`), and `spike_helm.jl`, including with `naut_core`.
  Rust, the brain CPU unit suite, and the separate
  [root CPU comparison](docs/cpu-comparison.md) need no GPU.

### 1. Structure + unit tests

```bash
./test_integration.sh
cd execution && cargo test          # fetches metabolic-ledger (+ optional feature deps)
cd ../brain && julia --project=. -e 'using Pkg; Pkg.instantiate()'
julia --project=. test/runtests.jl
julia --project=. test/backend_runtime.jl  # pinned imports + vault; 100-tick auto test on CUDA
# or: ./scripts/smoke_wire_local.sh
```

### 2. Health check (full GPU brain)

```bash
cd brain/
julia --project=. test_brain.jl
```

### 3. Synthetic research experiment (GPU; metrics + artifacts)

```bash
cd brain/
julia --project=. research_helm.jl --duration 200 --seed 42 --horizon 5
# Artifacts: runs/<run_id>/{meta.json,metrics.json}
# Options: --cost-bps 1.0 --output-dir runs --record-spikes --backend naut_core
```

`ExperimentRunner` (`experiment_runner.jl` + `metrics.jl`) generates a synthetic
random-walk price series with reservoir-dependent volatility. It records
future-horizon **hit_rate**, **PnL proxy**, and **latency p50/p99** on that
simulation, not historical-market walk-forward evaluation or evidence of a
tradable edge.
Reservoir façade picks **LiquidCortex** when loadable (`LIMEN_RESERVOIR` / `--backend`);
`--backend naut_core` selects the local CUDA ensemble, not a CPU fallback.
LiquidCortex is imported at file load time without exporting its names into the helm.
The backend runtime test reports GPU acceptance as skipped on CPU-only hosts.
MC fan-out stays **off** in research mode (`LIMEN_MC_PATHS` only for live helm).

### 4. Rust Decision path (policy gates + caller-sized ghost fills)

```bash
cd execution/
# Terminal A: MarketPulse feed on :5555 (the only in-tree publisher; the helm
# blocks until it sees a pulse, and the muscle refuses to fill without one)
cargo run --release --bin market_sim

# Terminal B: muscle — SUBs to readouts on :5556 and pulses on :5555
# Set LIMEN_KELLY_B to the experiment's expected gain / expected loss first.
# Example ONLY for synthetic smoke testing, NOT a calibrated market estimate:
LIMEN_KELLY_B=5 cargo run --release --bin limen-execution
# LIMEN_WIRE=json for legacy IPC
# LIMEN_CONFIDENCE_THRESHOLD=0.15
# LIMEN_KELLY_SCALAR=0.25
# LIMEN_MAX_ORDER_NOTIONAL=10     (quote units per order, NOT asset units)
# LIMEN_PULSE_MAX_AGE_MS=2000      (readouts are refused when the last pulse is older)
```

Fills are priced from the last `MarketPulse`. A readout that arrives before the
first pulse, after the pulse goes stale, or whose primary pair is RESIDUAL is
refused and logged — never filled at a placeholder price.

`ExecutionEngine::process_signal`: confidence gate → Neutral → fractional Kelly
from ATP bankroll → **cap order notional** → ghost fill. Oversized proposals are
capped, not rejected. The decision quantity, wallet mutation, and JSONL describe
the actual fill (buy ATP friction reduces units; sells cannot exceed inventory).
This is a per-order limit, not a cap on accumulated holdings.

`LIMEN_KELLY_B` is **required** and means expected gain / expected loss, not
percentage price movement (e.g. +2% versus −1% means `b=2`, not `0.02`).
No payoff estimate can be recovered from confidence alone or the current pulse
schema. All four sizing env values above fail startup on malformed, non-finite,
or out-of-range values. Threshold must be in `[0,1]`, scalar in `(0,1]`, and
payoff ratio and notional must be positive (notional must fit `f32`).

The **effective gate** is `p >= LIMEN_CONFIDENCE_THRESHOLD` **and**
`p > 1/(1+b)`, with non-neutral direction, valid price, sufficient ATP/inventory,
and a non-dust size. Zero Kelly gives `RejectedNoEdge`. Thus `b=0.05` still
requires `p > 0.952381`; the synthetic `b=5` example requires `p > 0.166667`
despite the 0.15 confidence threshold. Changing the fractional scalar does not
change the Kelly breakeven. SNN confidence is **not a calibrated win probability**.
Library-only `ExecutionEngine::new` uses symmetric `b=1`; the listener never
silently adopts that assumption.

The listener logs `Confidence summary:` JSON every 100 priced signals and on
graceful shutdown, including rejected/neutral signals: count, invalid count,
min/max/mean, ten confidence bins, and decision counts. Unpriced readouts and
invalid wire frames are excluded. Capture stdout over a sim run, then Ctrl+C to
include its final partial interval. The former “typical 0.2–0.4” claim was not
measured. See [the reproducible synthetic sim check](docs/results/rm-2440/README.md):
60/100 executions versus 0/100 at the old odds, **not** evidence about the real
brain's distribution or profitability.

**The accepted quantity is the filled quantity.** `process_signal` passes its
sized quantity — Kelly policy size by default, or `signal.quantity` under
`with_kelly_sizing(false)` — plus explicit simulated friction to
`apply_ghost_fill`; the ledger's ATP/Kelly-sizing `execute_buy`/`execute_sell`
helpers are not used on this path (RM-2439). `ExecutionDecision::Executed`
reports the ledger's rounded `f32` fill values. ADR 0001's
single-sizing-authority rule is enforced on this legacy ghost path; consuming
external sizing proposals (e.g. DendriteTrader `SizingModule`) remains future
work.

### 5. Julia live helm (GPU required)

```bash
cd brain/
LIMEN_SEED=42 julia --project=. spike_helm.jl
```

The live helm logs `LIMEN_SEED` (default `42`) and seeds both host and CUDA RNGs
before reservoir initialization. Replay also requires the same accepted pulse
sequence, configuration, backend, Julia/package versions and GPU stack.
`LIMEN_MC_PATHS=0` disables both MC paths and their covariance workload.

## Local notifications for `limen-live`

With `--features live-robinhood`, the paper loop sends one local desktop
notification per fill, stale-order cancel, decider error, or halt (daily-loss
cap or Ctrl-C). Normal holds and reaching `--max-ticks` do not notify.
Notifications identify paper mode, symbol, tick, event details, and journal path.
`journal.md` (or `--journal PATH`) remains the record; no hosted service is used.

On Linux, install `notify-send` (for example, the `libnotify-bin` package on
Debian/Ubuntu) and run in the operator's desktop session with its session D-Bus
environment (`DBUS_SESSION_BUS_ADDRESS`). An unattended user service must inherit
that environment and have a running desktop notification daemon. Verify delivery
from the same user/service environment with:

```bash
notify-send --app-name=limen-live -- "limen-live test" "Local notifications ready"
```

Missing helpers, unavailable desktop sessions, nonzero exits, or delivery taking
over five seconds produce a warning without interrupting trading or journaling.
There is no stderr-only fallback notification and no automatic retry. Headless
machines without a notification daemon (and non-Linux systems) need a compatible
local `notify-send` implementation before relying on these alerts.

## Signal Pipeline

### Input: 120-byte MarketPulse (from Rust → Julia)

```
[0..8]    timestamp_ns (UInt64)
[8..64]   7 assets × (price f32, vol f32), order from config/universe.toml
[64..80]  confidence_signal, funding_rate, liquidation_vol, liquidity_delta
[80..84]  l3_order_imbalance (market signal)
[84..100] GPU temp/power/util, Basys buffer load (hardware telemetry)
[100..120] OI delta, funding rate, reserved (108–120)
```

### Output: 88-byte readout (Julia → Rust)

```
[0..8]    tick_count (Int64)
[8..72]   16 × Float32 — aggregated lobe readout
[72..88]  4 × Float32 — NERO relevance [Scalper, Day, Swing, Macro]
```

## Math Modules (`FeatureStream`)

| Module | Purpose | Status |
|--------|---------|--------|
| `market_hawkes.jl` | Self-excitation intensity λ(t) | **Wired** via `feature_stream.jl` → encoder δ + Scalper reflex |
| `market_lsm.jl` | 256-neuron FastReflex | Optional (`LIMEN_USE_REFLEX=1`) |
| `market_fractal.jl` | Hurst / regime | Offline only (not on hot path) |
| `market_sde.jl` | GBM surprise z-score | Offline only (not on hot path) |

See `brain/feature_stream.jl`.

## Neural Dynamics

**OU-SDE Membrane Model:**
```
dV = ((V_rest - V) / τ_m + W_rec·S + W_in·u) dt + σ·dW
```

**Local `naut_core` readout STDP (RM-2443):**
```
ΔW_out[i,j] = η · (post[i] · trace_pre[j] - trace_post[i] · pre[j])
```

`post` is the current positive-readout event; traces decay before learning and
receive current events afterwards. Both potentiation and depression run every
10 ticks, with weights clamped to ±1. Recurrent initialization uses the random
matrix proxy `||W||_F / sqrt(N) ≈ 0.9`, not RMS nonzero weight. This estimate does
not guarantee echo-state stability. These changes invalidate earlier local-model
results. See the [fixed-seed before/after comparison](docs/brain-model-math.md).
The optional LiquidCortex backend keeps its pinned upstream learning rules.

**Global Inhibition ("Cortisol"):**
- GPU temp > 75°C → raise spike threshold
- High funding rate → "market is tense" → fewer spikes
- Liquidation cascade → inhibit all lobes

## License

**MIT** — see [`LICENSE`](LICENSE).

Optional Limen-Neural dependencies are typically MIT/Apache-2.0 as well.

## Repo hygiene

- Do not commit `execution/target/`, `runs/`, `data/`, `.env`, `.mimocode/`, or `security-scans/`.
- Golden wire fixtures live under `wire/fixtures/` and **are** tracked.

## Roadmap

- [x] Binary NervousWire (120/88) + golden fixtures
- [x] Reservoir façade + causal NERO + FeatureStream (Hawkes)
- [x] ExperimentRunner metrics artifacts
- [x] Decision path: fractional Kelly policy gates + caller-sized ghost execution
- [x] Enforce one sizing authority and pass accepted quantities to accounting on the legacy ghost path (ADR 0001, RM-2439)
- [ ] Consume external sizing proposals (e.g. DendriteTrader `SizingModule`) as the single authority (ADR 0001)
- [x] Git rev pins for validated library deps (no sibling clones)
- [ ] Replace JSON serialization with FlatBuffers (proto/signal.fbs)
- [ ] Add WebSocket streaming for dydx (replace REST polling)
- [ ] Fix CUDA RNG (remove host-side random number generation)
- [ ] Implement stop-loss and risk management gates in Rust
- [ ] Backtest framework with historical data

