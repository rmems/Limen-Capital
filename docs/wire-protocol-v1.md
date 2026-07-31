# Wire protocol v1 (research)

Single canonical path for experimental SNN-HFT between Julia brain and Rust muscle.

## Endpoints (defaults)

| Direction | Endpoint | Socket | Payload |
|-----------|----------|--------|---------|
| Rust → Julia | `tcp://127.0.0.1:5555` (`LIMEN_IPC_SUB`) | PUB/SUB | **MarketPulse** 120 bytes |
| Julia → Rust | `tcp://127.0.0.1:5556` (`LIMEN_IPC_PUB`) | PUB/SUB | **ReadoutPacket** 88 bytes |

Override via env. Research default is **plain ZMQ** (no CURVE). Set `ZMQ_CURVE=1` and supply real keys only when needed.

## MarketPulse — 120 bytes (little-endian)

| Offset | Type | Field |
|--------|------|-------|
| 0–7 | `u64` | `timestamp_ns` |
| 8–63 | 7 × (`f32` price, `f32` vol) | DNX, Quai, Qubic, Kaspa, Monero, Ocean, Verus |
| 64–67 | `f32` | `confidence_signal` |
| 68–71 | `f32` | `funding_rate` |
| 72–75 | `f32` | `liquidation_vol` |
| 76–79 | `f32` | `liquidity_delta` |
| 80–83 | `f32` | `l3_order_imbalance` |
| 84–87 | `f32` | `gpu_temp_c` |
| 88–91 | `f32` | `gpu_power_w` |
| 92–95 | `f32` | `gpu_util_pct` |
| 96–99 | `f32` | `basys_buffer_load` |
| 100–103 | `f32` | `dydx_oi_delta` |
| 104–107 | `f32` | `dydx_funding_rate` |
| 108–119 | reserved / future | currently unused in decoder |

Decoder lives in `brain/naut_core.jl` (`decode_market_pulse`) and Capital adapters.

## ReadoutPacket — 88 bytes (little-endian)

| Offset | Type | Field |
|--------|------|-------|
| 0–7 | `i64` | `tick_count` |
| 8–71 | 16 × `f32` | aggregated lobe readout |
| 72–87 | 4 × `f32` | relevance [Scalper, Day, Swing, Macro] |

Published by `spike_helm.jl` → `publish_readout!`.

## Deprecated / secondary paths

| Path | Status |
|------|--------|
| JSON over IPC (`signal_broadcaster.jl` + `LIMEN_WIRE=json`) | **Adapter only** — default `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` or `/tmp/limen-capital-$UID/signals.ipc` (mode `0700`); override `LIMEN_JSON_IPC` (`ipc://` + absolute path, no `..`). Side field is **lowercase** (`buy`/`sell`/`neutral`). |
| FlatBuffers `proto/signal.fbs` | Reserved for TradeSignal v2; not required for v1 binary path. |
| corpus-ipc models | Future shared schema; extend when packets stabilize. |

## Mapping readout → TradeSignal (v1 — implemented)

See `wire/README.md` and `corpus_ipc::readout_to_trade`:

1. 16 floats = 8 × (bull, bear); assets DNX…Verus + Residual.
2. `score_i = bull_i - bear_i`; primary = argmax |score|.
3. `confidence = clamp(max(relevance) * tanh(‖score‖₂), 0, 1)`.
4. Muscle: binary SUB on `LIMEN_IPC_PUB` → gates → **metabolic-ledger**.

Shared types: Capital `execution/src/binary_wire.rs` (`MarketPulse`, `ReadoutPacket`);
re-export for consumers when corpus-ipc publishes equivalent types.
Golden fixtures: `wire/fixtures/*.bin`.
