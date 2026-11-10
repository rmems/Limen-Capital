# Nervous Wire (v2)

Binary layouts for experimental SNN-HFT between Julia brain and Rust muscle. Symbol order comes from `config/universe.toml` (override with `LIMEN_UNIVERSE`).

**Docs:** [docs/wire-protocol-v2.md](../docs/wire-protocol-v2.md) (v2), [docs/wire-protocol-v1.md](../docs/wire-protocol-v1.md) (legacy fixed layout)  
**Rust types:** `execution/src/binary_wire/`, `execution/src/universe.rs`  
**Fixtures:** `fixtures/universe_3.toml`, `fixtures/universe_12.toml`; v1 golden `marketpulse.bin` / `readout.bin` for 7-symbol regression

## Endpoints

| Direction | Env | Default |
|-----------|-----|---------|
| Market → Brain | `LIMEN_IPC_SUB` | `tcp://127.0.0.1:5555` |
| Brain → Muscle | `LIMEN_IPC_PUB` | `tcp://127.0.0.1:5556` |

Endpoints must be loopback IPs or absolute `ipc://` paths unless
`LIMEN_ALLOW_REMOTE_WIRE=1`. Readout `tick` carries the source MarketPulse
timestamp; listeners refuse duplicate, stale, future, and pre-start frames.
See [docs/wire-protocol-v2.md](../docs/wire-protocol-v2.md#timestamps-replay-and-staleness).

## v2 frames

All v2 payloads prefix an 8-byte header (`LNRV`, version 2, frame type). Market and readout bodies include `symbol_table_hash` and variable `n_assets` / `n_pairs`. See the v2 doc for offsets.

## Readout → TradeSignal

```
readout = 2 × n_pairs floats (bull/bear)
pairs[0..n_assets-1] = universe symbols (runtime order)
pairs[n_assets]      = Residual (not tradeable)

score_i    = bull_i - bear_i
primary    = argmax_i |score_i|
confidence = clamp(max(relevance) * tanh(‖score‖₂), 0, 1)
ticker     = universe.symbol[primary] or RESIDUAL
```

Legacy v1 120 B / 88 B frames decode when the loaded universe has seven assets and eight pairs.
