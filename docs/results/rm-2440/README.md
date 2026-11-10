# RM-2440: execution plumbing simulation

This is a **synthetic readout fixture**, not calibration, a profitability result,
or a measurement of the Julia GPU brain's confidence distribution. No “typical”
real confidence range is established here. The GPU helm requires CUDA, unavailable
on the validation host.

## Reproduce

From the repository root:

```bash
cd execution
cargo test --locked --test wire_listener \
  market_sim_has_nontrivial_executions_and_logs_confidence_distribution -- --nocapture
```

The test starts the real `market_sim` and `limen-execution` binaries on private
IPC endpoints. Market pulses run at 50 Hz. Hand-built v2 readouts use fresh pulse
timestamps, alternate buy/sell on the first asset, and have relevance
`[0.4, 0.2, 0.2, 0.2]`. Their one active bull/bear value is
`atanh(target_confidence / 0.4)`. Ten targets
`[0.05, 0.10, 0.14, 0.16, 0.18, 0.22, 0.26, 0.30, 0.34, 0.38]`
repeat ten times. Prices come from `market_sim`, never a placeholder.

Each listener starts with 500 ATP, confidence threshold 0.15, scalar 0.25,
and maximum order notional 10. The only changed setting between runs is
`LIMEN_KELLY_B`. Both runs use the fixed implementation, with the old odds
reproducing the original no-edge symptom.

## Measured results (2026-11-10, Rust 1.99.1, Linux)

| Decision | `b=0.05` | `b=5` (explicit synthetic assumption) |
|----------|---------:|-------------------------------------:|
| Executed | 0 | 60 |
| RejectedLowConfidence | 30 | 30 |
| RejectedNoEdge | 70 | 10 |
| RejectedPositionLimit | 0 | 0 |
| Neutral | 0 | 0 |

Both runs log 100 priced signals, zero invalid confidence values, min **0.05**,
max **0.38**, mean **0.2130000051**. Decile counts (`[0,.1)`, …, `[.9,1]`) are
`[10,40,20,30,0,0,0,0,0,0]`. Both the periodic and graceful-shutdown summaries
are exercised.

The test asserts these results against the fixture's known targets, and checks
that all 60 `Executed` decisions produce JSONL fills with positive quantity and
gross notional **≤10**. At the old odds the effective gate exceeds every input;
at `b=5` it is `p > 1/6`, so six of ten targets can fill. This illustrates how
the configured odds affect the gate; **5 is not a recommended real-market ratio**.

Unit regressions separately check wallet quantities, decision quantities, audit
notional, repeated buy/sell operations, different prices and confidence levels,
invalid scalars/prices, empty inventory, exhausted ATP, and strict env parsing.
Binary integration checks verify bad env values fail before socket setup.

To measure the real brain, run the documented three-process setup with a
separately justified payoff ratio, capture the muscle's stdout, and stop it with
Ctrl+C. `Confidence summary:` reports all priced inputs before gates, so a
zero-fill run still exposes its score distribution. Unpriced/invalid wire
readouts are not part of this distribution. Do not select a payoff ratio after
looking at those scores simply to force trades.
