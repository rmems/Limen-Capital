# CPU reservoir comparison (RM-1865)

This is market-research adapter validation, not a profitability, production-HFT,
LIF-SNN, or cross-domain Spikenaut generalization claim.

## Reproduce

From the repository root, with Julia 1.12 or 1.13 and no GPU or credentials:

```sh
julia --project=. -e 'using Pkg; Pkg.instantiate(); Pkg.test()'
OPENBLAS_NUM_THREADS=1 julia --project=. scripts/compare_cpu.jl runs/rm-1865
```

Output must be a **new** directory. The CLI returns failure if any candidate fails;
`summary.json` retains every success/failure, not just a selected seed. An optional
second argument is a canonical DendriteTrader session JSONL path. It must contain
enough complete chronological observations for the same frozen split; it is not
automatically a qualified real-market dataset.

## Ownership and qualification

The root project pins DendriteTrader to
`16231a4e3898632418b6d2a764551c997f552d69` and LiquidCortex to
`2637b921ca3890086053e09f24636660a453cd34`. These are separate from the older
`brain/Project.toml` GPU-helm pins, which are unchanged. No sibling clones or path
dependencies are used. LiquidCortex's instance-owned `ReferenceLSM(gpu=false)` is
the first CPU candidate: 32 tanh reservoir units, five inputs, five projected
outputs. Qualification requires finite, nonzero outputs and exact reset replay.
There is no silent fallback to the local neural implementation.

`brain/reservoir.jl` remains the facade. LiquidCortex owns state evolution and
seeded weight construction. DendriteTrader owns the delta encoder, ridge training,
softmax/class mapping, causal features, labels, splits, and paper simulator.
Capital owns only feature mapping, train-fitted scale application, lifecycle,
campaign configuration, and reporting. The projected five-vector is wrapped in a
`FeatureRow` solely to consume the existing library readout, not to redefine
market feature semantics upstream. This reference reservoir is **not** a spiking
LIF implementation; optional GPU SNN qualification is future work.

## Frozen protocol

- Candidates: `StationaryModel`, `ImbalanceRule(0)`, `RidgeClassifier(1)`,
  reservoir with delta encoding, raw-input reservoir ablation, and delta reservoir
  with its membrane state zeroed before each row. The latter retains encoder
  history to isolate reservoir memory.
- All candidates run seeds **11, 22, 33**, including deterministic baselines.
  The synthetic data and fill model use independent fixed seed **9001**.
  Report mean and sample standard deviation, all individual outcomes, and failures.
  Three seeds are only a small variability diagnostic, not independent market samples.
- Inputs, in order: `spread_ticks`, `mid_ticks`, `microprice_ticks`, `imbalance`,
  `signed_order_flow`, computed by DendriteTrader from replay. Neither hardware
  telemetry nor hardware-derived inhibition is accepted. No confound experiment
  is enabled.
- Train: first **240** feature rows. Validation: next **100** after a four-event
  embargo. Test: next **120** after another four-event embargo. Horizon:
  **four exchange events**, movement threshold zero ticks, classes `(Down, Flat, Up)`.
  Exact row, sequence, and timestamp boundaries are in the manifest. Canonical
  contiguous event sequences are required; labels are built inside each range,
  dropping its final four anchors rather than reaching across boundaries.
- Adapter scales are maximum absolute values fitted on train only; the library
  ridge readout also fits its own scales only on projected train data. Reservoir
  weights are fixed, with no online plasticity. Delta threshold **0.05** in scaled
  feature units, saturation **1**, ridge penalty **1**. No hyperparameter search,
  calibration fit, threshold selection, or seed selection is performed. All choices
  are frozen before final evaluation; validation is diagnostic only.
- Zero reservoir and encoder state before training, validation, test, and the
  independent paper replay. No warmup, shuffled rows, or carryover across boundaries.
  Stateful inference rejects duplicate/backward sequences until reset. The first
  delta-encoded row is silent. `checkpoint_id` hashes weights, scales, seed,
  encoder/memory configuration and horizon, but not transient inference state.

## Forecast quality is not paper economics

Report accuracy, multiclass Brier score (sum across classes), log loss with a
declared numerical floor of `1e-15`, and five equal-width top-class reliability
bins / ECE. These are uncalibrated direction probabilities, **not trade win
probabilities**, and are never passed into Kelly.

All paper runs use the same conservative long/flat, fixed **one lot** policy,
one-lot inventory cap, one open order, **100,000 quote subunits** initial cash,
IOC at the observed opposite touch, and fixed research gate **0.6**. The gate is
not claimed to be a calibrated or production threshold. Order entry, cancellation,
and reporting latency are each **200,000 ns**, with receive-time market feed.
Fees are **100 ppm (1 bp)** per fill, positive costs applied once by the simulator,
rounded up in integer quote subunits. Extra taker slippage is zero ticks; crossing
spread costs remain in actual hypothetical fill prices. L2 queue fills are
approximate and exclude market impact.

All models predict every eligible test row, even while an order is pending.
Forecasts must exactly match an independently reset, event-by-event simulator
replay. Only test sequences may place orders; earlier events reconstruct the
causal book without warming the model. Replay ends at the test boundary, not at
unused trailing data. Paper summaries include fills, rejections, fees, turnover,
inventory, drawdown, and terminal marked equity, separately from forecast scores.
Every ledger step must reconcile before an outcome is accepted.

## Artifacts and limitations

Each run emits input events, frozen `manifest.json`, resolved `dependencies.toml`,
per-candidate forecasts, paper decision/fill JSONL, paper assumptions/metrics,
`summary.json`, and an artifact hash index. The manifest records code revision,
source-content hashes (so dirty-tree runs remain identifiable), dependency-manifest
hash, Julia/thread environment, dimensions, seeds and split boundaries.
The resolved dependency snapshot stays in the generated run directory and is
not committed; its recorded hash identifies the environment actually used.

[LIM-1035 / DendriteTrader #76](https://github.com/rmems/DendriteTrader.jl/issues/76)
is still open at the pinned revision. Reuse its available precursor artifacts:
the paper simulator's versioned `decision_records` and `fill_records`, preserving
decision/information/activation/fill/report clocks and IDs. Do not invent a
completed supervisor dataset or duplicate the pending generic exporter here.
The export does not yet attach LIM-1035's full future-return/MAE/MFE outcomes.
Spikenaut transfer/training remains a later consumer experiment.

### User-provided Spikenaut model

The [Spikenaut-SNN repository](https://github.com/rmems/Spikenaut-SNN) and
[Hugging Face model card](https://huggingface.co/rmems/Spikenaut-SNN), supplied
by Raul Montoya Cardenas during RM-1865, identify the live exp-025 bank as a
16-neuron LIF hardware-telemetry model. The Hub card identifies its artifact
lineage as GitHub `6965e12a6e46d29783e69f6b0fda3dda1058bd29`.
Its five active sensors are `mem_util_pct`, `power_w`, `gpu_temp_c`,
`sm_clock_mhz`, and `mem_clock_mhz` (axons 0–4, unused 5–15 held zero).
Its three outputs are regression channels `(comfort, temp, power)`, **not**
direction probabilities. Neither the old coin encoder nor a relabeling of these
outputs is a valid market adapter. No published weight is modified or evaluated
on market features in this campaign. A future transfer experiment needs its own
market encoder/readout training, train-only preprocessing, pinned checkpoint,
reset/quantization qualification, and fresh held-out evaluation. Hardware-only
health results do not establish market or cross-domain generalization.

The default fixture is only 484 synthetic book updates with unrealistic,
repeated removal/replacement of inside levels. It intentionally exercises state,
costs, latency, and null/negative outcomes, not market predictability. There is one
chronological split, small effective sample size, overlapping labels, and no
market-regime coverage. No calibrated sizing mapping is validated. Real historical
replay, a completed LIM-1035 dataset, and optional GPU qualification remain follow-up
experiments, not implicit claims of this run.

Calibration findings are recorded in the accompanying result report and in
[confidence-defaults.md](confidence-defaults.md) ([Capital #5](https://github.com/rmems/Limen-Capital/issues/5)).
They do not prescribe a magic production gate.

Recorded outcomes, all seed results and compact machine-readable evidence:
[results/rm-1865](results/rm-1865/README.md).

Planning attribution from the ticket: GPT-6 Astra Pro (ChatGPT/OpenAI), 2026-09-28.
That is planning provenance, not a claim that it implemented or executed this campaign.
