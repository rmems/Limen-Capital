# Nervous Wire (v1)

Canonical binary layouts for experimental SNN-HFT between Julia brain and Rust muscle.

**Source of truth (docs):** [docs/wire-protocol-v1.md](../docs/wire-protocol-v1.md)  
**Shared Rust types:** Limen-Neural `corpus-ipc` (`MarketPulse`, `ReadoutPacket`)  
**Golden fixtures:** `fixtures/marketpulse.bin` (120 B), `fixtures/readout.bin` (88 B)

## Endpoints

| Direction | Env | Default |
|-----------|-----|---------|
| Market → Brain | `LIMEN_IPC_SUB` | `tcp://127.0.0.1:5555` |
| Brain → Muscle | `LIMEN_IPC_PUB` | `tcp://127.0.0.1:5556` |

## MarketPulse (120 B LE)

| Offset | Type | Field |
|--------|------|-------|
| 0–7 | u64 | timestamp_ns |
| 8–63 | 7×(f32,f32) | DNX…Verus price, vol |
| 64–107 | 11×f32 | confidence, funding, liq, …, dydx |
| 108–119 | reserved | zero |

Assets (order): `DNX, Quai, Qubic, Kaspa, Monero, Ocean, Verus`.

## ReadoutPacket (88 B LE)

| Offset | Type | Field |
|--------|------|-------|
| 0–7 | i64 | tick |
| 8–71 | 16×f32 | lobe readout |
| 72–87 | 4×f32 | relevance trailer [Scalper, Day, Swing, Macro] |

**Trailer note:** `corpus-ipc::RuntimeSnapshot` is a *view adapter* over the same 4 floats (neuromod naming). HFT code should use `ReadoutPacket.relevance_*` / lobe names.

## Causal aggregation (C3)

Brain-side `agg = Σ_i relevance[i] · lobe_i.output` **before** packing the 16-float
block. Trailer relevance and readout are consistent (not static LOBE_WEIGHTS).

Env: `LIMEN_AGG_MODE=relevance|static|blend` (default `relevance`).

## Readout → TradeSignal (v1)

```
readout[16] = 8 × (bull, bear) pairs
pairs[0..6] = DNX, Quai, Qubic, Kaspa, Monero, Ocean, Verus
pairs[7]    = Residual

score_i    = bull_i - bear_i
primary    = argmax_i |score_i|
side       = Buy if score_primary > ε; Sell if < -ε; else Neutral
mag        = tanh(‖score‖₂)
confidence = clamp(max(relevance) * mag, 0, 1)
ticker     = asset name for primary (Residual → "RESIDUAL")
```

`ε` default = `1e-4`.

## Adapters

1. **Binary TCP** — research primary path  
2. **JSON IPC** — secondary (`LIMEN_JSON_IPC` or user-scoped default under `$XDG_RUNTIME_DIR/limen-capital/`, lowercase `side`)
