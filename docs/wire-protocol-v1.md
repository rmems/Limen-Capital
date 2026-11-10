# Wire protocol v1 (research)

Single canonical path for experimental SNN-HFT between Julia brain and Rust muscle.

## Endpoints (defaults)

| Direction | Endpoint | Socket | Payload |
|-----------|----------|--------|---------|
| Rust → Julia | `tcp://127.0.0.1:5555` (`LIMEN_IPC_SUB`) | PUB/SUB | **MarketPulse** 120 bytes |
| Julia → Rust | `tcp://127.0.0.1:5556` (`LIMEN_IPC_PUB`) | PUB/SUB | **ReadoutPacket** 88 bytes |

Override via env. Research default is **plain ZMQ** (no CURVE). Set `ZMQ_CURVE=1` and supply real keys via env only (`docs/SECURITY.md`).

Binary endpoints accept literal loopback TCP addresses (`127.0.0.0/8`, `[::1]`)
or absolute `ipc://` paths without `..`. Hostnames, wildcard binds, and remote
addresses require explicit `LIMEN_ALLOW_REMOTE_WIRE=1` on each peer. DNS names
are not resolved for local validation. Secure IPC parent directories yourself.

## Network envelope (required)

Each ZMQ message is **one frame**: an 8-byte header followed by the payload below.
Bytes 0–3 are ASCII `NW1P` (market) or `NW1R` (readout); byte 4 is version `1`;
bytes 5–7 are reserved zero. Network sizes are **128 B** and **96 B**, respectively.
Receivers reject unknown topics, versions, nonzero reserved bytes, and wrong sizes.
Bare 120/88-byte messages are no longer accepted on the network. Upgrade all
publishers and subscribers together; there is no legacy network fallback.

Readout has no reserved payload space, so the version lives in this common
header rather than overwriting a field. The `.bin` golden fixtures remain raw
payloads for offline codecs; `.frame` fixtures prepend the independently specified
8-byte header. All offsets in the tables below are relative to the payload.

With `ZMQ_CURVE=1`, both link directions use CURVE: market feed PUB (`market_sim` / `:5555`) and readout PUB (brain / `:5556`) act as CURVE servers; brain SUB and muscle SUB act as clients. Optionally restrict which client public keys may connect to a server by setting `ZMQ_CURVE_ALLOWLIST` or `ZMQ_CURVE_ALLOWLIST_FILE` (ZAP). Plain loopback without `ZMQ_CURVE=1` is unchanged.

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
| 108–119 | reserved / future | must be zero |

Decoder lives in `brain/naut_core.jl` (`decode_market_pulse`) and Capital adapters.

## ReadoutPacket — 88 bytes (little-endian)

| Offset | Type | Field |
|--------|------|-------|
| 0–7 | `i64` | originating MarketPulse Unix `timestamp_ns` (not a counter) |
| 8–71 | 16 × `f32` | aggregated lobe readout |
| 72–87 | 4 × `f32` | relevance [Scalper, Day, Swing, Macro] |

Published by `spike_helm.jl` → `publish_readout!`.

The muscle preserves this timestamp in `TradeSignal`, so `latency_ns` includes
market-to-brain wire time, inference, brain-to-muscle wire time, and queueing.
Both stream watermarks start at the muscle's startup Unix timestamp. Each
accepted timestamp must increase strictly, be no later than receipt time, and
be at most `LIMEN_PULSE_MAX_AGE_MS` old (default 2000 ms). This also refuses
pre-restart replays. A readout consumes its watermark even if no fresh price is
available, so replaying it after a pulse arrives cannot trade it.
The brain also refuses non-increasing, pre-start, and future market timestamps.
Peers must share a synchronized wall clock; backwards clock jumps fail closed
until timestamps catch up. Simulation counters are not network timestamps.

The listener owns the engine and last pulse and processes each frame inline,
without per-frame tasks or mutexes. ZMQ's bounded receive queues provide the
buffering; under overload PUB/SUB may drop frames, not accumulate unbounded
tasks. Ordering is receive order per publisher, not a total order across feeds.

Julia rejects non-finite readout/relevance before packing. NERO resets all
mutable history on non-finite input or derived arithmetic and skips that readout;
healthy subsequent lobes can resume without poisoned EMA history. Persistent
non-finite lobe state remains fail-closed, not silently converted to a trade.

## Deprecated / secondary paths

| Path | Status |
|------|--------|
| JSON over IPC (`signal_broadcaster.jl` + `LIMEN_WIRE=json`) | **Adapter only** — default `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` or `/tmp/limen-capital-$UID/signals.ipc` (mode `0700`); override `LIMEN_JSON_IPC` (`ipc://` + absolute path, no `..`). Side field is **lowercase** (`buy`/`sell`/`neutral`). |
| FlatBuffers `proto/signal.fbs` | Reserved for TradeSignal v2; not required for v1 binary path. |
| corpus-ipc models | Future shared schema; extend when packets stabilize. |

## Mapping readout → TradeSignal (v1 — implemented)

See `wire/README.md` and `execution/src/binary_wire.rs::readout_to_trade`:

1. 16 floats = 8 × (bull, bear); assets DNX…Verus + Residual.
2. `score_i = bull_i - bear_i`; primary = argmax |score|.
3. `confidence = clamp(max(relevance) * tanh(‖score‖₂), 0, 1)`.
4. Muscle: binary SUB on `LIMEN_IPC_PUB` → gates → **metabolic-ledger**.

Shared types: Capital `execution/src/binary_wire.rs` (`MarketPulse`, `ReadoutPacket`);
re-export for consumers when corpus-ipc publishes equivalent types.
Golden fixtures: `wire/fixtures/*.bin`.
