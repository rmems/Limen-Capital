# Confidence defaults and calibration boundaries (GitHub #5 / RM-1865)

This documents **research defaults** for how confidence-like scores flow through
Limen-Capital. It is not a production tuning guide, a profitability claim, or
permission to size live capital from uncalibrated model outputs.

## Two different “confidence” surfaces

| Surface | Meaning | Default gate | Kelly? |
|--------|---------|--------------|--------|
| **Direction-class probability** (`Forecast.probabilities`) | Uncalibrated multiclass scores for Down / Flat / Up from DendriteTrader readouts | **0.6** top-class threshold in the [CPU comparison](cpu-comparison.md) paper policy only | **No** — fixed one lot, long/flat |
| **SNN readout confidence** (`TradeSignal.confidence`, NervousWire v1) | Derived from aggregated lobe output + NERO relevance in `binary_wire.rs` | **`LIMEN_CONFIDENCE_THRESHOLD=0.15`** in the Rust muscle research path | **Yes** — fractional Kelly (see below), not validated as win probability |

Do not treat either surface as a calibrated **trade win probability** without a
separate, train-only calibration study and held-out evaluation.

## Rust muscle defaults (ghost / research)

- `ExecutionEngine::new(0.15, …)` — sub-threshold signals are rejected before sizing.
- The listener requires **`LIMEN_KELLY_B`**, an experiment-supplied expected
  gain / expected loss ratio, **not** percentage price movement. Neither a
  confidence score nor the current `MarketPulse` contains that estimate.
- Fractional Kelly defaults to **0.25×** (`LIMEN_KELLY_SCALAR`); per-order quote
  notional defaults to **10** (`LIMEN_MAX_ORDER_NOTIONAL`). Oversized orders are
  capped in the actual ghost fill, not rejected or re-sized by ledger Kelly.
- Positive Kelly requires **p > 1/(1+b)** as well as **p ≥ threshold**.
  `b=0.05` therefore requires `p > 0.952381`, regardless of a 0.15 threshold.
  The scalar changes size, not breakeven. Library convenience constructors use
  symmetric `b=1`; the executable always requires explicit odds.
- All sizing env values are strictly validated at startup. See `README.md` for
  ranges, dust/inventory gates, and summary logging.
- [RM-2440's synthetic sim check](results/rm-2440/README.md) measures the mapped
  confidence distribution and decision counts through the shipped listener and
  `market_sim`. It is **not** a measured distribution of the GPU brain.

## CPU comparison campaign defaults

Frozen in `src/comparison.jl` and [cpu-comparison.md](cpu-comparison.md):

- Paper policy: **0.6** on the **Up** / **Down** class probability (not NERVOUS-wire confidence).
- **No Kelly**, **one lot**, **long/flat**, identical costs/latency for all candidates.
- Forecast metrics (accuracy, Brier, log loss, ECE) are reported **separately** from paper fills/equity.

## RM-1865 calibration findings (synthetic fixture)

Recorded evidence: [results/rm-1865/README.md](results/rm-1865/README.md). Summary:

- **ImbalanceRule** reached the highest **accuracy** but very poor **calibration** (high ECE, extreme log loss on wrong one-hot peaks). High direction confidence does **not** imply a correct direction here.
- **StationaryModel** had lower ECE but no useful discriminative signal.
- **RidgeClassifier** and reservoir variants did not beat simple baselines in a way that justifies promoting any candidate after observing the held-out split.
- **No reservoir** candidate crossed the **0.6** paper gate (zero fills — a **null** outcome, not evidence of profitable abstention).

These findings **do not** prescribe a magic production threshold. They support
using conservative fixed policies until a sizing mapping is validated.

## Wire readout mapping (live helm path)

For the 16-element readout + 4 relevance scores, confidence is derived in Rust
(`execution/src/binary_wire.rs`) as roughly `clamp(max(relevance) * tanh(‖score‖₂), 0, 1)`.
Overflow/NaN inputs fail closed to **Neutral** with **zero** confidence.

Hardware telemetry fields on `MarketPulse` must **not** feed market features in
research runs (ADR 0001). The RM-1865 adapter explicitly sets `inhibition=0` and
excludes telemetry from the comparison campaign.

## Spikenaut / cross-domain note

The user-provided [Spikenaut-SNN](https://huggingface.co/rmems/Spikenaut-SNN)
checkpoint is a **hardware-telemetry regression** model (`comfort`, `temp`, `power`),
not direction classes. It is **out of scope** for this confidence contract until a
separate market encoder/readout transfer experiment is designed and qualified.

## Related issues

- GitHub **#5** — this document and the CPU comparison campaign.
- Linear **RM-1865** — adapter validation vs shared forecast baselines.
- **LIM-1035** — trajectory export for supervisor datasets (precursor simulator
  artifacts reused; full exporter still pending upstream).
