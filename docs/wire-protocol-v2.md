# Wire protocol v2 (market-neutral universe)

Runtime symbol tables replace hardcoded asset lists. Brain and muscle load the same `config/universe.toml` (or `LIMEN_UNIVERSE`) and reject frames whose `symbol_table_hash` does not match.

## Frame header (8 bytes, all v2 frames)

| Offset | Type | Field |
|--------|------|-------|
| 0–3 | `u32` | magic = `0x56524E4C` (`LNRV`) |
| 4 | `u8` | version = `2` |
| 5 | `u8` | frame type: `1` = MarketPulse, `2` = Readout |
| 6–7 | reserved | zero |

Frame typing uses magic/version — not payload length alone. Decoders reject
an unknown version or frame type, nonzero reserved bytes (header bytes 6–7 and
the `u16` after `n_assets` / `n_pairs`), and any length other than the exact
size implied by the count.

## MarketPulse v2

| Field | Type |
|-------|------|
| `timestamp_ns` | `u64` |
| `symbol_table_hash` | `u64` |
| `n_assets` | `u16` |
| reserved | `u16` |
| assets | `n_assets` × (`f32` price, `f32` vol) |
| trailing | 11 × `f32` (same semantics as v1) |

## ReadoutPacket v2

| Field | Type |
|-------|------|
| `tick` | `i64` source MarketPulse `timestamp_ns` (see below) |
| `symbol_table_hash` | `u64` |
| `n_pairs` | `u16` (`n_assets + 1`, last pair = residual) |
| reserved | `u16` |
| readout | `2 × n_pairs` × `f32` (bull/bear per pair) |
| relevance | 4 × `f32` NERO simplex |

## Legacy v1

Fixed 120 B / 88 B payloads without a header remain decodable when the loaded universe has 7 assets / 8 pairs (regression tests only). Production publishers (`market_sim`, brain helm) emit v2, and the brain and muscle listeners refuse header-less v1 frames on the network.

## Timestamps, replay, and staleness

The brain copies each MarketPulse `timestamp_ns` into the readout `tick` it
publishes for that pulse, and `TradeSignal.timestamp_ns` defaults to that
value, so measured latency includes wire and brain time.

Each listener keeps a per-stream watermark seeded with the wall clock at
startup. The brain (MarketPulse) and the muscle (MarketPulse and Readout) refuse
frames that are duplicate or out of order, older than `LIMEN_PULSE_MAX_AGE_MS`
(default 2000), later than the local clock, or from before the listener
started (for example, a brain tick counter from before a restart). A refused frame does not
move the watermark. These checks assume synchronized clocks; they are not
cryptographic anti-replay.

## Endpoints

`LIMEN_IPC_SUB` / `LIMEN_IPC_PUB` must be `ipc://` with an absolute path and no
`..` segments, or `tcp://` with a literal loopback IP (`127.0.0.0/8`, or `[::1]`; IPv6 must be bracketed)
and a port in 1–65535. Hostnames (including `localhost`), wildcards, and other
addresses need `LIMEN_ALLOW_REMOTE_WIRE=1`. The brain, muscle, and
`market_sim` all apply this check before connecting or binding.

## Non-finite values

Decoders reject non-finite prices, vols, trailing signals, readouts, and
relevance, and relevance outside [0, 1] or not summing to 1 ± 0.05. The brain
validates the same rules before publishing. When NERO state turns non-finite,
the brain resets NERO history and skips that tick's readout instead of
publishing NaN.

## Muscle processing

The muscle handles frames in arrival order on its listener thread (no
per-frame tasks or locks). A ZMQ poll or receive error ends the listener, and
the process exits with an error instead of idling.

## Trade mapping

Same as v1: `score_i = bull_i - bear_i`, primary = argmax |score|, confidence from NERO relevance × ‖score‖₂. Tickers resolve through the runtime symbol table (residual pair is not tradeable).
