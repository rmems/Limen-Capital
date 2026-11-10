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
│  Confidence / Kelly policy gates → ledger-sized fills    │
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
MC fan-out stays **off** in research mode (`LIMEN_MC_PATHS` only for live helm).

### 4. Rust Decision path (policy gates + ledger-sized ghost fills)

```bash
cd execution/
# Terminal A: MarketPulse feed on :5555 (the only in-tree publisher; the helm
# blocks until it sees a pulse, and the muscle refuses to fill without one)
cargo run --release --bin market_sim

# Terminal B: muscle — SUBs to readouts on :5556 and pulses on :5555
cargo run --release
# LIMEN_WIRE=json for legacy IPC
# LIMEN_CONFIDENCE_THRESHOLD=0.15  (initial research gate only, not the effective Kelly floor)
# LIMEN_PULSE_MAX_AGE_MS=2000      (readouts are refused when the last pulse is older)
```

Fills are priced from the last `MarketPulse`. A readout that arrives before the
first pulse, after the pulse goes stale, or whose primary pair is RESIDUAL is
refused and logged — never filled at a placeholder price.

`ExecutionEngine::process_signal`: confidence gate → Neutral filter →
**0.25× Kelly policy quantity** (from ATP bankroll, b=0.05) → max-qty gate →
ledger call. With default Kelly enabled, positive policy size requires
`p > 1 / (1 + 0.05) ≈ 0.952381`; passing 0.15 alone is insufficient.
Zero Kelly → `RejectedNoEdge`. SNN confidence is not a calibrated win probability.

**The policy quantity is not the filled quantity.** `execute_buy` / `execute_sell`
take no quantity: the pinned ledger buys from ATP × its adaptive Kelly fraction
(less metabolic cost), or sells that fraction of inventory. Rust's max-qty check
and `ExecutionDecision::Executed.quantity` describe the policy quantity, not an
enforced fill limit or confirmed fill. `with_kelly_sizing(false)` only switches
the policy gate to `signal.quantity`; it does not bypass ledger sizing. Exact
upstream-sized fills and ADR 0001's single-sizing-authority rule remain unimplemented.

### 5. Julia live helm (GPU required)

```bash
cd brain/
julia --project=. spike_helm.jl
```

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
- [x] Decision path: fractional Kelly policy gates + ledger-sized ghost execution
- [ ] Enforce one sizing authority and pass accepted quantities to accounting (ADR 0001)
- [x] Git rev pins for validated library deps (no sibling clones)
- [ ] Replace JSON serialization with FlatBuffers (proto/signal.fbs)
- [ ] Add WebSocket streaming for dydx (replace REST polling)
- [ ] Fix CUDA RNG (remove host-side random number generation)
- [ ] Implement stop-loss and risk management gates in Rust
- [ ] Backtest framework with historical data

