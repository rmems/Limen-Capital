# RM-1865: recorded CPU comparison

Recorded 2026-11-09, Julia 1.13.3, Linux x86_64, CPU `icelake-server`,
one Julia thread and one BLAS thread. Protocol: [CPU comparison](../../cpu-comparison.md).
Revalidated on origin/main `28ae81e` (2026-11-10): all 18 numerical outcomes
remain unchanged from the original run. Confidence/calibration boundaries:
[confidence-defaults.md](../../confidence-defaults.md).

**Outcome: adapter qualified; no evidence of a useful temporal-reservoir advantage.**
All six preregistered candidates completed all three seeds (18/18, zero failures).
This is synthetic plumbing evidence, not a market profitability or production claim.

## Sample and provenance

484 synthetic book updates yield 483 complete feature rows. Train:
sequences 2–241 (240 features, 236 labels); validation: 246–345 (100/96);
test: 350–469 (120/116). Four-event embargo at both boundaries; four-event
forecast horizon. Trailing four labels per range are unavailable, not fabricated.
The final 15 feature rows are unused. Test class counts: Down 39, Flat 38, Up 39.
The same input, splits, costs, policy, latency and zero-search budget apply to every run.

- Data SHA-256: `4286628e1f7a2c9d8df7775a7a1bb0d0e0bc5ef2f06670095830a710c09f35ba`.
- [manifest.json](manifest.json): frozen configuration, qualification, boundaries,
  environment, seed list, source-content hashes, and dependency hash.
- The exact resolved dependency manifest is generated as `dependencies.toml` in
  each run directory. Its hash remains in `manifest.json`; the snapshot is not
  committed, following the user's approved removal after a Droid-Shield flag.
- [summary.json](summary.json): **all 18 outcomes**, validation and held-out scores,
  reliability-bin counts/sums, checkpoint hashes, paper summaries, and aggregate variability.
- [artifact_hashes.json](artifact_hashes.json): complete raw-run artifact inventory.

The run preceded the implementation commit. `code_revision` therefore names its
parent checkout; the manifest's source-content hashes identify the actual code
executed, and match the source files in this PR. Do not mistake the parent commit
alone for the implementation identity.

To regenerate full per-row features/targets/forecasts, versioned simulator
decisions/fills, audit rows, and per-step ledgers in a new directory:

```sh
OPENBLAS_NUM_THREADS=1 julia --project=. scripts/compare_cpu.jl runs/rm-1865-replay
```

Raw generated JSONL is intentionally not committed (about 6.7 MB total artifacts).
The inventory describes the recorded run, not files all present in this compact
evidence directory. Code-revision metadata changes on a new checkout, so its
manifest/hash need not be byte-identical; numerical outcomes should match with
the recorded dependency manifest and environment. Without that local manifest,
the pinned library revisions remain reproducible, but registry dependency
versions may resolve differently.

## Held-out forecast quality

Mean ± sample standard deviation over seeds 11/22/33; **not** a market-sample
confidence interval. Repeated deterministic baselines have exactly zero seed
variance. Lower Brier/log loss/ECE is better. No fitted calibration was applied.

| Candidate | Accuracy | Brier | Log loss | ECE |
|---|---:|---:|---:|---:|
| StationaryModel | .3276 ± .0000 | .6751 ± .0000 | 1.1107 ± .0000 | .0750 ± .0000 |
| ImbalanceRule | .5862 ± .0000 | .8276 ± .0000 | 14.2919 ± .0000 | .4138 ± .0000 |
| RidgeClassifier | .5517 ± .0000 | .5876 ± .0000 | .9858 ± .0000 | .1479 ± .0000 |
| Reservoir, delta | .4425 ± .0249 | .5909 ± .0024 | .9908 ± .0033 | .2094 ± .0223 |
| Reservoir, raw | .5316 ± .0217 | .5800 ± .0152 | .9754 ± .0207 | .1366 ± .0108 |
| Reservoir, no memory | .4569 ± .0000 | .5910 ± .0019 | .9911 ± .0027 | .2018 ± .0292 |

All reservoir seed outcomes (no seed selected):

| Candidate | Seed | Accuracy | Brier | Log loss | ECE |
|---|---:|---:|---:|---:|---:|
| Delta | 11 | .4569 | .5886 | .9875 | .1847 |
| Delta | 22 | .4569 | .5907 | .9906 | .2157 |
| Delta | 33 | .4138 | .5934 | .9942 | .2280 |
| Raw | 11 | .5517 | .5784 | .9732 | .1489 |
| Raw | 22 | .5345 | .5657 | .9559 | .1292 |
| Raw | 33 | .5086 | .5959 | .9972 | .1317 |
| No memory | 11 | .4569 | .5891 | .9882 | .1843 |
| No memory | 22 | .4569 | .5912 | .9914 | .2355 |
| No memory | 33 | .4569 | .5928 | .9936 | .1856 |

The delta reservoir trails RidgeClassifier on accuracy, Brier and log loss.
Removing reservoir memory hardly changes Brier/log loss and slightly improves
accuracy: a null/negative temporal-state finding. Raw inputs improve mean
probabilistic scores but vary more between seeds; their accuracy does not beat
RidgeClassifier. No ablation is promoted after observing test results.

The imbalance rule's one-hot probabilities achieve the highest accuracy but poor
calibration and very high log loss (wrong predictions hit the declared `1e-15`
numerical floor). StationaryModel's smaller ECE does not imply useful predictions.
These findings are relevant to [Capital #5](https://github.com/rmems/Limen-Capital/issues/5):
class confidence is not validated trade win probability, and this experiment
does not justify a production confidence threshold or Kelly fraction.

## Separate paper economics

Identical fixed one-lot, long/flat policy, research gate .6, 200,000 ns entry/
cancel/report delays. Initial cash 100,000 quote subunits; price scale 100,
quantity scale 1. Nominal fees 100 ppm (1 bp), commission rounded up once per fill;
no extra taker slippage ticks, but crossing spread remains in prices.

| Candidate | Fills | Fees (subunits) | Turnover (subunits) | Final equity | Max drawdown |
|---|---:|---:|---:|---:|---:|
| StationaryModel | 0 | 0 | 0 | 100,000 | 0 |
| ImbalanceRule | 58 | 58 | 5,836 | 99,880 | 121 |
| RidgeClassifier | 0 | 0 | 0 | 100,000 | 0 |
| Reservoir, delta | 0 | 0 | 0 | 100,000 | 0 |
| Reservoir, raw | 0 | 0 | 0 | 100,000 | 0 |
| Reservoir, no memory | 0 | 0 | 0 | 100,000 | 0 |

All paper values have zero variance across the three seeds. The imbalance rule
submitted 59 orders: 58 filled, one expired, none rejected; terminal inventory was
zero. Its equity change was **−120 subunits**, including 58 subunits of fees.
Integer rounding dominates the nominal fee on these tiny synthetic notionals
(effective fees about 99.4 bp of turnover, not 1 bp). This is a fixture limitation,
not a real-market fee estimate. Other candidates never crossed the fixed gate:
zero fills is a **null** result, not evidence of profitable abstention, a sizing
mapping, or a defined trade win rate.

Every accepted run passed offline-versus-simulator forecast equality and
independent per-step accounting reconciliation. No fill preceded decision plus
declared entry latency. L2 fills remain approximate, with no market impact.

## Validation and remaining limits

- `julia --project=. -e 'using Pkg; Pkg.test()'`: **344 passed**.
- CLI comparison: **18 completed, zero failed**.
- Independent Python oracle reconstructed midpoint movements from raw book deltas
  and verified labels/scores for all 18 outcomes, source/data hashes, fill counts,
  exact fee application and fill latency.
- Updated brain CPU suite: **51 passed**, including the TemporalFocus load probe.
  The initial rerun lacked the newly required ZMQ environment resolution; after
  `JULIA_PKG_PRECOMPILE_AUTO=0 julia --project=brain -e 'using Pkg; Pkg.resolve()'`,
  the documented brain test command passed without source changes.
- Rust fmt, all-feature clippy with warnings denied, and locked tests: pass
  (37 tests, one live-network test ignored).
- Structure gate: **25 passed**. Its first cold build attempt timed out at 60 s;
  the rerun after compilation passed.
- GPU qualification, live feeds, and FPGA parity: not run (no GPU/board required).
- Beads tracking was unavailable during the original run; the refreshed upstream
  instructions have since removed that workflow.

The library emits expected no-GPU and upstream CUDA deprecation warnings. The CPU
reference backend loads and passes regardless; no GPU result is implied.
LIM-1035's full trajectory exporter and future-return/MAE/MFE annotations are
still pending upstream; this run reuses the simulator's available precursor
records rather than pretending that dataset exists.

One short synthetic split, overlapping horizon labels, three initialization seeds,
and no real historical-market data cannot establish statistical significance,
profitability, deployment readiness, or cross-domain transfer. The user-provided
[Spikenaut model](https://huggingface.co/rmems/Spikenaut-SNN) remains a
hardware-telemetry artifact with `(comfort, temp, power)` regression outputs;
its weights were not remapped to market classes or modified.
