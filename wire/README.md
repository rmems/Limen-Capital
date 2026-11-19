# Nervous Wire (v2)

Binary layouts for experimental SNN-HFT between Julia brain and Rust muscle. Symbol order comes from `config/universe.toml` (override with `LIMEN_UNIVERSE`).

**Docs:** [docs/wire-protocol-v2.md](../docs/wire-protocol-v2.md) (v2), [docs/wire-protocol-v1.md](../docs/wire-protocol-v1.md) (legacy fixed layout)  
**Rust types:** `execution/src/binary_wire/`, `execution/src/universe.rs`  
**Fixtures:** `fixtures/universe_3.toml`, `fixtures/universe_12.toml`; v1 golden `marketpulse.bin` / `readout.bin` for 7-symbol regression

## Executable contract checks

Run from the repository root:

```bash
python3 wire/generate_fixtures.py          # check independent Python struct oracle
python3 wire/generate_fixtures.py --write  # deliberately regenerate committed bytes
julia --project=brain brain/test/runtests.jl
cargo test --manifest-path execution/Cargo.toml --locked --test wire_fixtures
bash scripts/test_julia_rust_wire.sh
```

Both languages compare their production v2 writers byte-for-byte with
`marketpulse_v2.bin` and `readout_v2.bin`, and assert every decoded field.
Legacy v1 writers (where available) and all v1 decoded fields are also pinned.
Expected values are literal inputs, not derived by decoding and re-encoding.
Missing or changed fixtures fail the tests. The generator uses explicit
little-endian `struct` layouts, independently of either implementation.

The required **Julia → Rust wire decisions** CI job invokes the Julia packers
in a fresh process, then Rust decodes those files, maps signals, resolves prices,
and calls `ExecutionEngine::default()` (confidence 0.15, max quantity 10).
It checks Buy/Sell policy quantities, actual wallet deltas and JSONL records,
plus confidence/no-edge rejections without wallet or log changes. This test is
explicitly ignored in standalone `cargo test`, which needs no Julia; the runner
selects it with `--ignored` and fails if the generated files are absent.
All trade logs and transient wire files live in auto-cleaned temporary directories.

### Execution limitations made visible

The historical golden readout has confidence about 0.244. It passes the default
confidence gate but has **no Kelly edge** with payoff ratio 0.05. The default
decision is therefore `RejectedNoEdge`, not an execution. Executable tests pin
this decision rather than relaxing production thresholds to get a fill.

Policy quantity is still advisory to `metabolic-ledger`: a new wallet buys
using 8% of ATP and sells 8% of held units, with 0.1% friction, independently
of the policy quantity returned by `process_signal`. Tests explicitly pin both
numbers (for example, at price 10 and confidence 0.98, policy quantity is about
7.25 while the actual buy fill is 3.996). Fixing that sizing contract is separate
from these test improvements.

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
