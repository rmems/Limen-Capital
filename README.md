# Limen-Capital — Experimental SNN-HFT (Julia Brain + Rust Muscle)

Hybrid research stack: **Julia neuromorphic strategy** + **Rust deterministic execution**.
Built for experimental spiking / liquid-state market research — **not live capital**.

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
| **LiquidCortex.jl** | Preferred CUDA LSM (optional; else `naut_core`) |
| **TemporalFocus** (NeuroPulse.jl) | Optional relevance package |
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
│  TemporalFocus / NERO relevance                         │
│       ↓                                                 │
│  16-element readout + 4 relevance scores                │
└──────────────────────┬──────────────────────────────────┘
                       │ 88-byte binary over ZMQ TCP (v1)
                       ↓
┌──────────────────────────────────────────────────────────┐
│  Rust Muscle                                             │
│  Confidence gates → fractional Kelly → metabolic-ledger  │
│  optional dydx feed → latency tracking → JSONL           │
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
- Optional: NVIDIA GPU for full EnsembleBrain / research runs

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

### 3. Research experiment (metrics + artifacts)

```bash
cd brain/
julia --project=. research_helm.jl --duration 200 --seed 42 --horizon 5
# Artifacts: runs/<run_id>/{meta.json,metrics.json}
# Options: --cost-bps 1.0 --output-dir runs --record-spikes --backend naut_core
```

`ExperimentRunner` (`experiment_runner.jl` + `metrics.jl`) records walk-forward **hit_rate**, **PnL proxy**, and **latency p50/p99**.  
Reservoir façade picks **LiquidCortex** when loadable (`LIMEN_RESERVOIR` / `--backend`).  
LiquidCortex is imported at file load time without exporting its names into the helm.
The backend runtime test reports GPU acceptance as skipped on CPU-only hosts.
MC fan-out stays **off** in research mode (`LIMEN_MC_PATHS` only for live helm).

### 4. Rust Decision path (gates + fractional Kelly + metabolic-ledger)

```bash
cd execution/
# Terminal A: MarketPulse feed on :5555 (the only in-tree publisher; the helm
# blocks until it sees a pulse, and the muscle refuses to fill without one)
cargo run --release --bin market_sim

# Terminal B: muscle — SUBs to readouts on :5556 and pulses on :5555
cargo run --release
# LIMEN_WIRE=json for legacy IPC
# LIMEN_CONFIDENCE_THRESHOLD=0.15  (research default in main)
# LIMEN_PULSE_MAX_AGE_MS=2000      (readouts are refused when the last pulse is older)
```

Fills are priced from the last `MarketPulse`. A readout that arrives before the
first pulse, after the pulse goes stale, or whose primary pair is RESIDUAL is
refused and logged — never filled at a placeholder price.

`ExecutionEngine::process_signal`: confidence gate → Neutral → **0.25× Kelly size**
(from ATP bankroll, b=0.05) → max-quantity/holdings gate → quantity-taking ghost
fill. The ledger applies the accepted quantity once, without its adaptive Kelly
resizing it. `with_kelly_sizing(false)` uses `signal.quantity` instead.
The limit bounds each fill and total long holdings per asset. Prices and
quantities are rounded to the legacy ledger's `f32` units; `Executed` reports
those actual fill values. Zero Kelly → `RejectedNoEdge`. Insufficient holdings
(including selling while flat), insufficient ATP for quantity plus friction,
invalid fills, or JSONL write failures → `RejectedFill`, with no wallet mutation.
Buy friction enters cost basis and is realized on sale, so round-trip PnL and
win-rate statistics include both sides' friction exactly once.

### 5. Julia live helm (binary wire v1)

```bash
cd brain/
julia --project=. spike_helm.jl
```

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

**STDP Covariance Learning:**
```
ΔW = η · trace_pre · trace_post
```

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
- [x] Decision path: fractional Kelly + metabolic-ledger
- [x] Git rev pins for validated library deps (no sibling clones)
- [ ] Replace JSON serialization with FlatBuffers (proto/signal.fbs)
- [ ] Add WebSocket streaming for dydx (replace REST polling)
- [ ] Fix CUDA RNG (remove host-side random number generation)
- [ ] Implement stop-loss and risk management gates in Rust
- [ ] Backtest framework with historical data

