# Limen-Capital — Experimental SNN-HFT (Julia Brain + Rust Muscle)

Hybrid research stack: **Julia neuromorphic strategy** + **Rust deterministic execution**.
Built for experimental spiking / liquid-state market research — **not live capital**.

This repo is a **standalone application** (your personal/org remote). It is **not**
published under the Limen-Neural GitHub organization. It *depends on* validated
git+rev library pins (most now under `rmems/`; `neuromod` remains on Limen-Neural);
in-tree `naut_core` works without them.

## Dependencies (git pins only)

Validated packages are pulled by **`git` + `rev`** (Cargo / Julia
`[sources]`). No sibling `Limen-Neural/` checkout is required.

| Package | Role |
|---------|------|
| **metabolic-ledger** | Ghost wallet, ATP gates, JSONL (git pin) |
| **LiquidCortex.jl** | Preferred CUDA LSM (optional; else `naut_core`) |
| **TemporalFocus** (NeuroPulse.jl) | Optional relevance package |
| **binary_wire** (in-tree) | MarketPulse 120B / ReadoutPacket 88B until corpus-ipc publishes them |

See **[docs/deps.md](docs/deps.md)**, **[docs/wire-protocol-v1.md](docs/wire-protocol-v1.md)**,
and **[docs/SECURITY.md](docs/SECURITY.md)**.

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

- Julia **1.12+** (Pkg `[sources]` pins; CI uses 1.12), Rust **stable** (latest), network for first-time git deps
- Optional: NVIDIA GPU for full EnsembleBrain / research runs

### 1. Structure + unit tests

```bash
./test_integration.sh
cd execution && cargo test          # fetches metabolic-ledger (+ optional feature deps)
cd ../brain && julia --project=. -e 'using Pkg; Pkg.instantiate()'
julia --project=. test/runtests.jl
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
MC fan-out stays **off** in research mode (`LIMEN_MC_PATHS` only for live helm).

### 4. Rust Decision path (gates + fractional Kelly + metabolic-ledger)

```bash
cd execution/
cargo run --release
# Binary ReadoutPacket on :5556 by default; LIMEN_WIRE=json for legacy IPC
# LIMEN_CONFIDENCE_THRESHOLD=0.15  (research default in main)
```

`ExecutionEngine::process_signal`: confidence gate → Neutral → **0.25× Kelly size**
(from ATP bankroll, b=0.05) → max-qty gate → ghost fill. Zero Kelly → `RejectedNoEdge`.

### 5. Julia live helm (binary wire v1)

```bash
cd brain/
julia --project=. spike_helm.jl
```

## Signal Pipeline

### Input: 120-byte MarketPulse (from Rust → Julia)

```
[0..8]    timestamp_ns (UInt64)
[8..64]   7 assets × (price f32, vol f32) = DNX,Quai,Qubic,Kaspa,XMR,Ocean,Verus
[64..80]  funding_rate, liquidation_vol, liquidity_delta, order_imbalance
[80..100] GPU temp/power/util, FPGA buffer load
[100..120] dydx OI delta, dydx funding rate, Qubic fields
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

