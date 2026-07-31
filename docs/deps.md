# Limen-Capital external dependencies

This repository is **not** part of the Limen-Neural GitHub organization. It is a
standalone research app that **consumes** Limen-Neural libraries (and local
`brain/` / `math/` code).

**Default layout for local path deps** (sibling directories):

```text
parent/
  Limen-Capital/     ← this repo
  Limen-Neural/      ← library checkouts (or set LIMEN_NEURAL)
    metabolic-ledger/
    corpus-ipc/
    LiquidCortex.jl/
    NeuroPulse.jl/   # package name: TemporalFocus
    ...
```

Quality policy: prefer Limen-Neural packages that have tests + a clear API.
Use **path** deps for multi-repo development on one machine. For a machine
without siblings, switch to `git` + `rev` pins against public Limen-Neural URLs
(see below) — still keeping **this** repo under your personal/org remote.

## Rust (`execution/Cargo.toml`)

| Crate | Role | Local path | Status |
|-------|------|------------|--------|
| **metabolic-ledger** | Ghost wallet, ATP gates, JSONL | `$LIMEN_NEURAL/metabolic-ledger` | **Required** |
| **corpus-ipc** | MarketPulse 120B, ReadoutPacket 88B | `$LIMEN_NEURAL/corpus-ipc` | **Required** |
| kinetic-signals | Streaming Hawkes / surprise | optional feature `kinetic` | Optional |
| neuromod | Reference LIF/STDP | optional feature `snn` | Optional |

Cargo currently uses relative paths `../../Limen-Neural/...` from `execution/`.
If your layout differs, set `LIMEN_NEURAL` and adjust paths or use `[patch]`.

### Example git pins (when not using sibling trees)

```toml
metabolic-ledger = { git = "https://github.com/Limen-Neural/metabolic-ledger", rev = "<pin>" }
corpus-ipc = { git = "https://github.com/Limen-Neural/corpus-ipc", rev = "<pin>" }
```

## Julia (`brain/Project.toml`)

| Package | Role | Local path | Status |
|---------|------|------------|--------|
| **LiquidCortex** | Sparse CUDA LSM | `$LIMEN_NEURAL/LiquidCortex.jl` | Preferred |
| **TemporalFocus** | Relevance routing | `$LIMEN_NEURAL/NeuroPulse.jl` | Preferred |
| CUDA / ZMQ / … | Runtime | registry | Required |

**Naming note:** package `TemporalFocus` lives in the **NeuroPulse.jl** repo.

```julia
# Path develop (local)
pkg> develop $(ENV["LIMEN_NEURAL"])/LiquidCortex.jl
pkg> develop $(ENV["LIMEN_NEURAL"])/NeuroPulse.jl
```

Fallback: `LIMEN_RESERVOIR=naut_core` uses in-tree `brain/naut_core.jl` without LiquidCortex.

## Environment

| Variable | Meaning | Default |
|----------|---------|---------|
| `LIMEN_NEURAL` | Root of library checkouts | `../Limen-Neural` relative to Capital |
| `LIMEN_VAULT_DIR` | DuckDB vault directory | `data/vault` under Capital |
| `LIMEN_IPC_SUB` | MarketPulse PUB endpoint | `tcp://127.0.0.1:5555` |
| `LIMEN_IPC_PUB` | ReadoutPacket PUB endpoint | `tcp://127.0.0.1:5556` |
| `LIMEN_RESERVOIR` | `auto` / `liquid_cortex` / `naut_core` | `auto` |
| `LIMEN_AGG_MODE` | `relevance` / `static` / `blend` | `relevance` |
| `LIMEN_MC_PATHS` | Monte Carlo paths (0 = off) | `0` |
| `LIMEN_USE_REFLEX` | FastReflex preprocessor | `0` |
| `LIMEN_WIRE` | `binary` / `json` on muscle | `binary` |
| `LIMEN_CONFIDENCE_THRESHOLD` | Execution gate | `0.15` (main) |
| `ZMQ_CURVE` | Enable CURVE (`1`) | `0` |
| `ZMQ_SERVER_KEY` / `ZMQ_CLIENT_KEY` | CURVE material | empty (required if CURVE on) |

## License note

Limen-Capital is **MIT** (see root `LICENSE`). Limen-Neural libraries are typically
MIT/Apache-2.0 — compatible with this project license.
