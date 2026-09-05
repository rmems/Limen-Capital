# Limen-Capital external dependencies

This repository is **not** part of the Limen-Neural GitHub organization. It is a
standalone research app that **consumes** validated git+rev library pins (and local
`brain/` / `math/` code).

**Policy:** only **validated** packages, as **`git` + `rev` pins**. Transferred
libraries resolve from `github.com/rmems/...`. `neuromod` remains at
`Limen-Neural/neuromod` (no rmems transfer). No sibling path clones
(`../Limen-Neural/...`) are required or supported as the default workflow.

Quality bar: tests + a clear package boundary + API stable enough for experimental use.

## Rust (`execution/Cargo.toml`)

| Crate | Role | Pin (rev) | Status |
|-------|------|-----------|--------|
| **metabolic-ledger** | Ghost wallet, ATP gates, JSONL | `91822f842c13…` (2026-07-11) | **Required** (`rmems`) |
| kinetic-signals | Streaming Hawkes / surprise | `ccc883107e67…` (2026-08-30) | Optional feature `kinetic` (`rmems`) |
| neuromod | Reference LIF/STDP | `2a548da6006f…` | Optional feature `snn` (`Limen-Neural`; no rmems home) |
| corpus-ipc | Shared IPC models | — | **Not yet:** public main lacks MarketPulse/ReadoutPacket; Capital owns `execution/src/binary_wire.rs` |

```toml
metabolic-ledger = { git = "https://github.com/rmems/metabolic-ledger", rev = "91822f842c13b0a2b5d8d7b75160933fab2459d6" }
kinetic-signals = { git = "https://github.com/rmems/kinetic-signals", rev = "ccc883107e6763969179f036ac33ae22dffdc865", optional = true }
neuromod = { git = "https://github.com/Limen-Neural/neuromod", rev = "2a548da6006fedb732b07491b69023476b0cc339", optional = true }
```

`cargo test` / `cargo build` will fetch these over the network on first resolve.

## Julia (`brain/Project.toml`)

Requires **Julia 1.12+** (`[compat] julia = "1.12"`). Pkg `[sources]` git pins need
Julia ≥ 1.11; CI and local research target **1.12**.

| Package | Role | Pin (rev) | Status |
|---------|------|-----------|--------|
| **LiquidCortex** | Sparse CUDA LSM | `edb3570ffdbc…` (2026-08-31) | Preferred (`rmems`) |
| **TemporalFocus** | Relevance routing (repo **NeuroPulse.jl**) | `ac4aa2ca4c28…` (2026-09-05) | Preferred (`rmems`) |
| CUDA / ZMQ / … | Runtime | registry | Required |

```toml
[sources]
LiquidCortex = {url = "https://github.com/rmems/LiquidCortex.jl", rev = "edb3570ffdbcbc7d631e5a5f990dc82d50b228d7"}
TemporalFocus = {url = "https://github.com/rmems/NeuroPulse.jl", rev = "ac4aa2ca4c28e63b7a9d2980f5d27348629476b3"}
```

```bash
cd brain && julia --project=. -e 'using Pkg; Pkg.instantiate()'
```

Fallback: `LIMEN_RESERVOIR=naut_core` uses in-tree `brain/naut_core.jl` without LiquidCortex.

## Environment

| Variable | Meaning | Default |
|----------|---------|---------|
| `LIMEN_VAULT_DIR` | DuckDB vault directory | `data/vault` under Capital |
| `LIMEN_IPC_SUB` | MarketPulse PUB endpoint | `tcp://127.0.0.1:5555` |
| `LIMEN_IPC_PUB` | ReadoutPacket PUB endpoint | `tcp://127.0.0.1:5556` |
| `LIMEN_JSON_IPC` | JSON TradeSignal IPC (`ipc://` + absolute path) | `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` or `/tmp/limen-capital-$UID/signals.ipc` |
| `LIMEN_RESERVOIR` | `auto` / `liquid_cortex` / `naut_core` | `auto` |
| `LIMEN_AGG_MODE` | `relevance` / `static` / `blend` | `relevance` |
| `LIMEN_MC_PATHS` | Monte Carlo paths (0 = off) | `0` |
| `LIMEN_USE_REFLEX` | FastReflex preprocessor | `0` |
| `LIMEN_WIRE` | `binary` / `json` on muscle | `binary` |
| `LIMEN_CONFIDENCE_THRESHOLD` | Execution gate | `0.15` (research default) |
| `ZMQ_CURVE` | Enable CURVE (`1`) | `0` |
| `ZMQ_SERVER_KEY` / `ZMQ_CLIENT_KEY` | CURVE material | empty (required if CURVE on) |

See also **[docs/SECURITY.md](SECURITY.md)**.

## Explicit non-deps

Do not add path or git deps for silicon-*, brainstem-daemon, engram-parser,
cortex-tensor, hybrid-fusion, thalamic-relay, or retired `spikenaut-*` crates
unless quality-validated for a concrete Capital role.

## License note

Limen-Capital is **MIT** (see root `LICENSE`). Limen-Neural libraries are typically
dual MIT/Apache-2.0 — check each package’s license file when redistributing.
