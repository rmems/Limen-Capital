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
- `PositionSizer` uses **0.25× fractional Kelly** and payoff ratio **b = 0.05** by default (`execution/src/kelly.rs`, `execution/src/lib.rs`).
- With **b = 0.05**, positive policy Kelly requires `p > 1 / (1 + b) ≈ 0.952381`.
  The **0.15** threshold is only the initial gate; passing it does not imply a
  nonzero policy quantity. SNN confidence is not calibrated as **p**.
- This is a **policy gate**, not exact fill sizing. The pinned ledger helpers
  accept no quantity and apply their own adaptive Kelly to ATP/inventory;
  Rust's policy quantity and max-quantity gate do not constrain actual fills.
  ADR 0001's one-authority rule remains unimplemented.
- Override via environment: `LIMEN_CONFIDENCE_THRESHOLD` (documented in `README.md`).

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
