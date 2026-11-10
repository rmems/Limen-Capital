# Limen-Capital external dependencies

This repository is **not** part of the Limen-Neural GitHub organization. It is a
standalone research app that **consumes** validated library releases and pins (and local
`brain/` / `math/` code).

**Policy (RM-2120):** use validated **crates.io releases** for `kinetic-signals`,
`neuromod`, `corpus-ipc`, `axon-encoder`, and `plasticity-lab`, with resolved
versions and registry checksums committed in `execution/Cargo.lock`.
`metabolic-ledger` and Julia libraries retain their validated **`git` + `rev`
pins**. No sibling path clones
(`../Limen-Neural/...`) are required or supported as the default workflow.

Quality bar: tests + a clear package boundary + API stable enough for experimental use.

## Rust (`execution/Cargo.toml`)

Requires **Rust 1.99**; CI and environment setup scripts pin **1.99.0**.
There is no repository toolchain file.

| Crate | Role | Release / pin | Status |
|-------|------|-----------|--------|
| **metabolic-ledger** | Ghost wallet, ATP gates, JSONL | `91822f842c13…` (2026-07-11) | **Required** (`rmems`) |
| kinetic-signals | Streaming Hawkes / surprise | crates.io `0.5.0` | Optional feature `kinetic` |
| neuromod | Reference LIF/STDP | crates.io `0.7.0` | Optional feature `snn` |
| corpus-ipc | Typed IPC models / optional ZMQ transport | crates.io `0.2.1` | Optional feature `corpus-ipc`; runtime migration deferred |
| axon-encoder | Rust spike encoder candidate | crates.io `0.6.0` | Optional feature `axon-encoder`; Julia MarketEncoder unchanged |
| plasticity-lab | Reward-modulated STDP session owner | crates.io `0.2.1` | Optional feature `plasticity-lab`; train_ghost wiring deferred |

```toml
metabolic-ledger = { git = "https://github.com/rmems/metabolic-ledger", rev = "91822f842c13b0a2b5d8d7b75160933fab2459d6" }
kinetic-signals = { version = "0.5.0", optional = true }
neuromod = { version = "0.7.0", optional = true }
corpus-ipc = { version = "0.2.1", optional = true }
axon-encoder = { version = "0.6.0", optional = true }
plasticity-lab = { version = "0.2.1", optional = true }
```

`cargo test --locked` / `cargo build --locked` fetch these over the network on
first use. Version requirements use Cargo's compatible-release semantics; the
lockfile records the exact versions above. All five dependencies are optional,
and `default = []` remains unchanged. CI builds and tests the default and each
library feature independently, and runs clippy with all features.

This is dependency adoption only: `zmq` and `execution/src/binary_wire.rs` remain
the active IPC path, Julia still owns the 28-spike `MarketEncoder`, and
`train_ghost` does not yet call plasticity-lab. The `corpus-ipc` feature currently
enables its typed core only; its upstream `zmq` transport feature is not enabled.
`plasticity-lab` depends on **neuromod 0.6.0**, alongside the direct optional
**0.7.0** dependency. Do not pass version-specific neuromod types across that
boundary without a follow-up compatibility design. Its optional `critic` feature
is not enabled, so `limbic-critic` is not adopted.

### kinetic-signals compatibility review

Compared the old git pin `ccc883107e6763969179f036ac33ae22dffdc865`
(pre-publication 0.4.0) with the published 0.5.0 source commit
`3eb02dd7269b214a57bcf7fed219a87577f52969` (from the downloaded crate's
`.cargo_vcs_info.json`), including `src/`, `Cargo.toml`, and the release changelog:

- Removed the optional `sentry` feature and `init_sentry()` API; observability
  belongs in the application.
- Added snapshot/restore APIs for EMA, SMA, and VolEstimator, snapshot types and
  constants, optional `serde`, and caller-owned output APIs
  `compute_surprise_sequence_into`, `compute_shannon_entropy_into`, and
  `surprise_sequence_len`. New prelude exports may collide with other glob imports.
- `SMA::new` and `VolEstimator::new` now panic above capacity **1,000,000**;
  entropy requests above **1,000,000** bins return the empty sentinel.
- Non-finite, ill-conditioned, overflow, and non-monotonic Hawkes inputs now use
  documented finite sentinels / preserved streaming state. SMA/stats use Welford
  accumulation and volatility squares in f64, so numeric edge behavior changes.
- MSRV rises from the old pin's 1.85 to **1.98.1**, covered by execution's 1.99.

Execution has no Rust call sites or prelude imports for kinetic-signals yet,
so no application API migration is needed. These differences matter when wiring
the feature into the signal path later.

## Julia (`brain/Project.toml`)

Requires **Julia 1.12 or 1.13** (`[compat] julia = "~1.12, ~1.13"`). Pkg `[sources]` git
pins need Julia ≥ 1.11; CI and local research target **1.13** (latest stable).

| Package | Role | Pin (rev) | Status |
|---------|------|-----------|--------|
| **LiquidCortex** | Sparse CUDA LSM | `edb3570ffdbc…` (2026-08-31) | Preferred (`rmems`) |
| **TemporalFocus** | Load probe only (repo **NeuroPulse.jl**); helms use in-tree NERO | `ac4aa2ca4c28…` (2026-09-05) | Pinned, not wired into helms |
| CUDA / ZMQ / … | Runtime | registry | Required |

```toml
[sources]
LiquidCortex = {url = "https://github.com/rmems/LiquidCortex.jl", rev = "edb3570ffdbcbc7d631e5a5f990dc82d50b228d7"}
TemporalFocus = {url = "https://github.com/rmems/NeuroPulse.jl", rev = "ac4aa2ca4c28e63b7a9d2980f5d27348629476b3"}
```

```bash
cd brain && julia --project=. -e 'using Pkg; Pkg.instantiate()'
```

Fallback: `LIMEN_RESERVOIR=naut_core` uses in-tree `brain/naut_core.jl` without
LiquidCortex, but still requires CUDA in both helms. It is not a CPU fallback.
`brain/adapters.jl` is not included by the helms, so its TemporalFocus load probe
does not provide runtime relevance routing.

## Environment

### Root CPU research project

The root `Project.toml` pins **DendriteTrader**
`16231a4e3898632418b6d2a764551c997f552d69` (forecast/split/simulator interfaces)
and **LiquidCortex** `2637b921ca3890086053e09f24636660a453cd34`
(instance-owned CPU `ReferenceLSM`). These separate research pins do not change
the older CUDA-helm environment above. The CPU candidate still loads CUDA.jl as a
library dependency, but needs no device or driver. See [cpu-comparison.md](cpu-comparison.md)
for qualification, commands, and limitations.

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
| `ZMQ_*_SERVER_SECRET` / `ZMQ_SERVER_KEY` | CURVE server bind secrets | empty (required on server sockets if CURVE on) |
| `ZMQ_CLIENT_SECRET_KEY` / `ZMQ_CLIENT_PUBLIC_KEY` | CURVE client identity | empty (required on client sockets if CURVE on) |
| `ZMQ_MARKET_SERVER_PUBLIC_KEY` / `ZMQ_READOUT_SERVER_PUBLIC_KEY` | Remote server public (Z85) | empty |
| `ZMQ_CURVE_ALLOWLIST` / `ZMQ_CURVE_ALLOWLIST_FILE` | Optional ZAP client public-key allowlist | unset |
| `ZMQ_ZAP_DOMAIN` | ZAP domain for server sockets | `limen-capital` |

See also **[docs/SECURITY.md](SECURITY.md)**.

## Explicit non-deps

Do not add path or git deps for silicon-*, brainstem-daemon, engram-parser,
cortex-tensor, hybrid-fusion, thalamic-relay, or retired `spikenaut-*` crates
unless quality-validated for a concrete Capital role.

## License note

Limen-Capital is **MIT** (see root `LICENSE`). Limen-Neural libraries are typically
dual MIT/Apache-2.0 — check each package’s license file when redistributing.
