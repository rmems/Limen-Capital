# RM-2443: deliberate local-model dynamics reset

Earlier `naut_core` results are **not comparable** to results after this change.
Restart with fresh weights and traces; regenerate any research baselines rather
than interpreting the new readout as a continuation of the old model.

## Changes

1. Recurrent weights use `||W||_F / sqrt(N)` as the random, zero-mean matrix
   spectral-radius proxy. The previous `||W||_F / sqrt(nnz)` measured RMS weight.
   The local reservoir actually has **65,536 neurons per lobe**, not 8,192
   (8,192 is the covariance subsample). At 1% COO connectivity, approximately
   652 distinct nonzeros per row remain after duplicate coalescing. Thus the
   old nominal 0.9 proxy implied a radius near `0.9 * sqrt(652) ≈ 23`.
   Accumulation now uses Float64, including for Float16 sparse weights.
   Logging calls this a **proxy**, not an eigenvalue bound or a guarantee of
   echo-state stability. Structured or nonzero-mean matrices need an actual
   spectral analysis instead.
2. `W_out` uses balanced pair STDP, with one post trace per **output**:

   ```text
   output = W_out * S
   post = Float32(output > 0)                    # elementwise
   trace_pre *= (1 - DT / TAU_TRACE)
   trace_post *= (1 - DT / TAU_TRACE)
   every 10 ticks:
       W_out += eta * (post * trace_pre' - trace_post * S')
       clamp W_out to [-1, 1]
       output = W_out * S
   trace_pre += S
   trace_post += post
   ```

   Past pre activity potentiates a current output event; past output activity
   depresses a current pre event. Simultaneous activity alone does not ratchet
   weights up. The positive-output event uses the **current**, not stale, readout.
   The Scalper flash-learning rate multiplies both terms. Traces still advance
   every tick, even when weights are not updated.
3. NERO has separate raw and inhibited score buffers. Momentum is the absolute
   difference between the last two normalized relevance vectors. History is
   committed only after all lobes have been scored successfully. Reset and
   finite-state guards cover the new buffers.
4. The live helm accepts unsigned 64-bit `LIMEN_SEED`, default **42**, logs it,
   and seeds the host task RNG and CUDA RNG before reservoir/encoder creation.
   Bad seeds fail startup. Replaying requires identical accepted input order,
   universe/configuration, backend, MC/reflex settings, versions and GPU stack;
   a seed alone cannot replay an unrecorded feed or promise cross-device
   bitwise determinism. The timestamp guard still refuses stale network frames.
5. Covariance runs only when `LIMEN_MC_PATHS > 0` and its interval is due.
   MC remains an optional diagnostic workload; its path buffer is not a trading
   signal. With MC disabled, none of the four dense covariance matrices is
   computed. This change does not introduce an MC trading policy.
6. Liquidity and Hawkes reflex signals are combined by greatest absolute
   magnitude, retaining the selected sign (liquidity wins ties). A withdrawal
   shock now reaches the existing absolute-value flash-learning threshold.

The local model fixes apply to `LIMEN_RESERVOIR=naut_core`. The optional pinned
LiquidCortex implementation is not modified. Seed, covariance and reflex fixes
apply to the live helm whichever backend it selects.

## Reproducible before/after readout comparison

Run from the repository root:

```sh
julia --startup-file=no --project=brain brain/compare_model_math.jl
julia --startup-file=no --project=brain brain/test/runtests.jl
```

Recorded with Julia **1.13.3**, CPU, one BLAS thread, seed **2443**,
4 lobes × **256** presynaptic neurons, 16 outputs and **2,000** ticks.
The CPU-only harness freezes the old initialization, readout update and
zero-momentum semantics from
`93cc0f26684e7df5ab2744097af1fcb7bada1494`. The corrected side invokes the same
generic readout helper used by `naut_core.step!` and the real NERO implementation.
Both receive identical initialized readout weights and externally generated
spike sequences, with varying lobe engagement. Relevance aggregation is forced
to `relevance` mode. Both use eta 0.001, trace decay 0.995, and 10-tick learning.

| Metric | Before | After |
|---|---:|---:|
| Measured recurrent spectral radius (separate seeded sparse matrix) | 4.214311 | 0.949945 |
| Mean absolute aggregated readout | 15.178750 | 2.106697 |
| Fraction of weights at magnitude ≥ 0.999 | 1.000000 | 0.005066 |
| Individual weight-decrease events | 0 | 258,922 |
| Mean relevance momentum, L1/tick | 0.000000 | 0.117333 |

Mean absolute before/after aggregated readout difference: **17.248321**.
All 16 final old outputs are **11.043792**, reproducing the spike-count
collapse. Final corrected outputs are distinct, from **-2.587289** to
**-1.361545**. Negative readouts are permitted by the existing signed-weight
model; these numbers are evidence of changed behavior, **not** evidence of
better forecasts or profitability.

The radius experiment uses a separate random 256×256 matrix with 8% COO
connectivity and Float16 scaling. Its eigenvalues are measured directly on the
CPU, independently of the proxy. The corrected radius is not exactly 0.9,
which demonstrates the proxy's finite-size limitation.

**Scope:** this is a fixed-spike **readout** replay, not a GPU OU-SDE reservoir
simulation, market backtest, ghost-trade replay, or full-size stability proof.
The recurrent matrix experiment is separate and does not drive this spike tape.
A full GPU experiment needs to be rerun on the target hardware with the fixed
seed and recorded accepted pulses before using these results for model selection.
No full GPU run was possible in the CPU-only validation environment.

CPU tests also exercise exact-eigenvalue Hadamard fixtures, causal and
anti-causal learning, repeated simultaneous activity, non-learning ticks,
clamping, multi-tick NERO history and poisoned-buffer recovery, seed logging
and replay, MC-disabled intervals, and negative liquidity shocks. The default
comparison is run twice and checked for exact replay in the CPU unit suite.
