# Wire protocol v2 (market-neutral universe)

Runtime symbol tables replace hardcoded asset lists. Brain and muscle load the same `config/universe.toml` (or `LIMEN_UNIVERSE`) and reject frames whose `symbol_table_hash` does not match.

## Frame header (8 bytes, all v2 frames)

| Offset | Type | Field |
|--------|------|-------|
| 0–3 | `u32` | magic = `0x56524E4C` (`LNRV`) |
| 4 | `u8` | version = `2` |
| 5 | `u8` | frame type: `1` = MarketPulse, `2` = Readout |
| 6–7 | reserved | zero |

Frame typing uses magic/version — not payload length alone.

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
| `tick` | `i64` |
| `symbol_table_hash` | `u64` |
| `n_pairs` | `u16` (`n_assets + 1`, last pair = residual) |
| reserved | `u16` |
| readout | `2 × n_pairs` × `f32` (bull/bear per pair) |
| relevance | 4 × `f32` NERO simplex |

## Legacy v1

Fixed 120 B / 88 B payloads without a header remain decodable when the loaded universe has 7 assets / 8 pairs (regression tests only). Production publishers (`market_sim`, brain helm) emit v2.

## Trade mapping

Same as v1: `score_i = bull_i - bear_i`, primary = argmax |score|, confidence from NERO relevance × ‖score‖₂. Tickers resolve through the runtime symbol table (residual pair is not tradeable).
