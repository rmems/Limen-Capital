# nero_orchestrator.jl — NERO: Neuromorphic Evaluation of Relevance and Orchestration
#
# NERO manages a 4-node static graph over the EnsembleBrain lobes and computes
# a per-tick relevance score for each lobe based on two signals:
#
#   1. Spike Density  — normalised firing rate of the lobe (0..1)
#      A lobe that is firing actively is "engaged" with current market regime.
#
#   2. Manifold Surprise — how much the lobe's current readout deviates from its
#      recent exponential moving average (EMA).  High surprise → the lobe just
#      transitioned into a new attractor / regime.
#
# Final relevance = α × spike_density + β × manifold_surprise + γ × ema_momentum
# All scalars are Float32; no heap allocation in the hot path.
#
# **Causal aggregation (C3):** after `update_relevance!`, call
# `aggregate_with_relevance!(ensemble, nero.relevance)` so wire readout is
#   agg = Σ_i relevance[i] · lobe_i.output
# not static LOBE_WEIGHTS. Relevance on the wire then matches the decision vector.
#
# Wire contract (appended to the existing 72-byte Julia readout):
#   [72..76]  relevance[1] → Scalper  (Float32 LE)
#   [76..80]  relevance[2] → Day      (Float32 LE)
#   [80..84]  relevance[3] → Swing    (Float32 LE)
#   [84..88]  relevance[4] → Macro    (Float32 LE)
#
# ─────────────────────────────────────────────────────────────────────────────

# using Graphs  # Temporarily disabled to fix dependency issue

# ── NERO Tuning Constants ─────────────────────────────────────────────────────

const NERO_ALPHA = 0.50f0        # Weight: spike density contribution
const NERO_BETA  = 0.35f0        # Weight: manifold surprise contribution
const NERO_GAMMA = 0.15f0        # Weight: readout EMA momentum
const NERO_EMA_DECAY = 0.05f0    # EMA smoothing factor (5% new estimate per tick)
const NERO_SURPRISE_TAU = 20.0f0 # Surprise normalisation window (tick-equivalent)
const NERO_MIN_SCORE = 0.01f0    # Clamp: never let any lobe drop to zero relevance
const NERO_EPSILON = 1.0f-6    # Numerical stability floor for normalisation

# Cross-lobe inhibition: adjacency weight from fast→slow lobes
# Layout: NERO_INHIBIT[from_lobe, to_lobe].  Scalper inhibits Day/Swing/Macro
# when it spikes heavily, de-emphasizing slow lobes during HFT regimes.
const NERO_INHIBIT = Float32[
    0.0   0.08  0.05  0.02;   # Scalper → {Day, Swing, Macro}
    0.04  0.0   0.06  0.03;   # Day     → {Scalper, Swing, Macro}
    0.02  0.03  0.0   0.05;   # Swing   → {Scalper, Day, Macro}
    0.01  0.02  0.03  0.0     # Macro   → {Scalper, Day, Swing}
]

# ── NERO State ────────────────────────────────────────────────────────────────

"""
    NeroOrchestrator

Holds all mutable state for NERO's per-tick relevance computation.
Pre-allocated at startup; the hot-path `update_relevance!` does NO heap
allocation — all work is in-place on these fields.

Fields:
  graph          — Graphs.SimpleDiGraph with 4 nodes (one per lobe).
                   Built once; never mutated during inference.
  relevance      — Current 4-element relevance vector (output to wire packet).
  readout_ema    — Per-lobe EMA of the 16-element readout vector.
"""
mutable struct NeroOrchestrator
    adjacency_matrix::Matrix{Float32}  # 4×4 adjacency matrix for cross-lobe inhibition
    relevance::Vector{Float32}         # Current relevance scores (sum = 1.0)
    readout_ema::Matrix{Float32}       # EMA of each lobe's readout (4×16)
    spike_density::Vector{Float32}     # Current spike density per lobe
    prev_relevance::Vector{Float32}    # Previous tick relevance (for momentum)
    temp_buffer::Vector{Float32}       # Temporary buffer (reused to avoid allocation)
    surprise::Vector{Float32}          # Per-lobe manifold surprise
    tick_count::Int64
end

"""
    NeroOrchestrator() -> NeroOrchestrator

Build the static lobe graph and pre-allocate all working buffers.
Called once at brain startup alongside `EnsembleBrain()`.
"""
function NeroOrchestrator()
    # Simple adjacency matrix for fully-connected directed graph (4 nodes, 12 directed edges)
    # adjacency_matrix[i,j] = 1 if there's an edge from i to j, 0 otherwise
    adjacency_matrix = zeros(Float32, N_LOBES, N_LOBES)
    for i in 1:N_LOBES, j in 1:N_LOBES
        if i != j
            adjacency_matrix[i,j] = 1.0f0
        end
    end

    NeroOrchestrator(
        adjacency_matrix,
        fill(0.25f0, N_LOBES),       # Start with equal relevance
        zeros(Float32, N_LOBES, N_OUT),
        zeros(Float32, N_LOBES),
        fill(0.25f0, N_LOBES),
        zeros(Float32, N_OUT),
        zeros(Float32, N_LOBES),       # surprise
        Int64(0)
    )
end

# ── Core Update ───────────────────────────────────────────────────────────────

"""
    update_relevance!(nero, ensemble) -> nothing

Compute NERO relevance scores for all 4 lobes from the current ensemble state.
The result is stored in `nero.relevance` (4 × Float32) and can be appended
to the ZMQ readout packet with zero additional allocation.

Algorithm per lobe i:
  1. spike_density[i]  = lobe.last_spike_rate (already maintained by step!)
  2. readout_ema[i,:]  = (1-EMA_DECAY) × old_ema + EMA_DECAY × lobe.output_cpu
  3. surprise[i]       = norm(readout_delta) / (norm(readout_ema) + ε)
  4. momentum[i]       = |relevance[i] - prev_relevance[i]|
  5. raw[i]            = α×density + β×surprise + γ×momentum

Cross-lobe inhibition via graph edges:
  6. inhibited[i] = raw[i] - Σⱼ NERO_INHIBIT[j,i] × raw[j]

Softmax normalisation → sum(relevance) = 1.0, each ≥ NERO_MIN_SCORE.
"""
function update_relevance!(nero::NeroOrchestrator, ensemble)
    nero.tick_count += 1

    # ── Save previous relevance for momentum computation ───────────────────
    # Must happen BEFORE raw aliases prev_relevance below
    copyto!(nero.prev_relevance, nero.relevance)

    # ── Stage 1-3: per-lobe signal collection ─────────────────────────────
    raw = nero.prev_relevance   # reuse buffer for raw scores

    for i in 1:N_LOBES
        lobe = ensemble.lobes[i]

        # 1. Spike density (already a Float32 in [0,1])
        spike_density = lobe.last_spike_rate

        # 2. Readout EMA update (in-place; no new array)
        copyto!(nero.temp_buffer, Array(lobe.output))

        @views ema_row = nero.readout_ema[i, :]
        ema_row .= (1.0f0 - NERO_EMA_DECAY) .* ema_row .+
                    NERO_EMA_DECAY .* nero.temp_buffer

        # 3. Manifold surprise: |new - ema| / (|ema| + ε)
        nero.temp_buffer .-= ema_row
        delta_norm = norm(nero.temp_buffer)
        ema_norm   = norm(ema_row) + NERO_EPSILON
        nero.surprise[i] = delta_norm / ema_norm

        # 4. Momentum: how much our relevance estimate changed last tick
        momentum = abs(nero.relevance[i] - nero.prev_relevance[i])

        # 5. Raw score
        raw[i] = NERO_ALPHA * spike_density +
                 NERO_BETA  * nero.surprise[i] +
                 NERO_GAMMA * momentum
    end

    # ── Stage 4: cross-lobe graph inhibition ──────────────────────────────
    # inhibited[i] = raw[i] - Σⱼ≠ᵢ NERO_INHIBIT[j,i] × raw[j]
    # Uses the adjacency matrix for cross-lobe inhibition
    inhibited = nero.relevance   # reuse as output buffer
    for dst in 1:N_LOBES
        inh_sum = 0.0f0
        for src in 1:N_LOBES
            if nero.adjacency_matrix[src, dst] > 0.0f0
                inh_sum += NERO_INHIBIT[src, dst] * raw[src]
            end
        end
        inhibited[dst] = max(raw[dst] - inh_sum, NERO_MIN_SCORE)
    end

    # ── Stage 5: softmax normalisation ──────────────────────────────────
    # Standard numerically stable softmax
    max_val = maximum(inhibited)
    s = 0.0f0
    for i in 1:N_LOBES
        inhibited[i] = exp(inhibited[i] - max_val)
        s += inhibited[i]
    end
    inhibited ./= (s + NERO_EPSILON)

    # Enforce minimum score (re-normalise after clamping)
    for i in 1:N_LOBES
        if inhibited[i] < NERO_MIN_SCORE
            inhibited[i] = NERO_MIN_SCORE
        end
    end
    reinorm = sum(inhibited)
    inhibited ./= (reinorm + NERO_EPSILON)

    return nothing
end

# ── Causal aggregation ────────────────────────────────────────────────────────

"""
    weighted_readout(lobe_outputs, relevance) -> Vector{Float32}

CPU pure helper: Σ_i (w_i / Σw) · out_i. Used by tests and offline analysis.
"""
function weighted_readout(
    lobe_outputs::Vector{<:AbstractVector{<:Real}},
    relevance::AbstractVector{<:Real},
)::Vector{Float32}
    n = length(lobe_outputs)
    length(relevance) == n || error("relevance length $(length(relevance)) ≠ n_lobes $n")
    n >= 1 || error("need ≥1 lobe")
    dim = length(lobe_outputs[1])
    s = sum(Float32(r) for r in relevance)
    s = max(s, NERO_EPSILON)
    out = zeros(Float32, dim)
    for i in 1:n
        w = Float32(relevance[i]) / s
        for j in 1:dim
            out[j] += w * Float32(lobe_outputs[i][j])
        end
    end
    return out
end

"""
    aggregate_with_relevance!(ensemble, relevance) -> nothing

Overwrite `ensemble.agg_output` with relevance-weighted sum of per-lobe readouts.
Call **after** `update_relevance!` so published readout is causal on NERO.

Env `LIMEN_AGG_MODE`:
  - `relevance` (default) — use `relevance` weights
  - `static` — keep static LOBE_WEIGHTS (ensemble.weights); no-op here if already stepped
  - `blend` — 0.5·static + 0.5·relevance
"""
function aggregate_with_relevance!(ensemble, relevance::AbstractVector{<:Real})
    mode = lowercase(get(ENV, "LIMEN_AGG_MODE", "relevance"))
    lobes = ensemble.lobes
    n = length(lobes)
    length(relevance) == n || error("relevance length $(length(relevance)) ≠ n_lobes $n")

    if mode == "static"
        # Re-apply static weights only
        ensemble.agg_output .= 0
        for (i, lobe) in enumerate(lobes)
            ensemble.agg_output .+= ensemble.weights[i] .* lobe.output
        end
    elseif mode == "blend"
        rel = Float32.(relevance)
        s = max(sum(rel), NERO_EPSILON)
        rel ./= s
        ensemble.agg_output .= 0
        for (i, lobe) in enumerate(lobes)
            w = 0.5f0 * ensemble.weights[i] + 0.5f0 * rel[i]
            ensemble.agg_output .+= w .* lobe.output
        end
    else
        # relevance (default)
        rel = Float32.(relevance)
        s = max(sum(rel), NERO_EPSILON)
        ensemble.agg_output .= 0
        for (i, lobe) in enumerate(lobes)
            ensemble.agg_output .+= (rel[i] / s) .* lobe.output
        end
    end
    return nothing
end

"""
    route_and_aggregate!(nero, ensemble) -> Vector{Float32}

Update NERO from ensemble state, re-aggregate with relevance, return CPU readout.
"""
function route_and_aggregate!(nero::NeroOrchestrator, ensemble)
    update_relevance!(nero, ensemble)
    aggregate_with_relevance!(ensemble, nero.relevance)
    return Array(ensemble.agg_output)
end

# ── Diagnostics ───────────────────────────────────────────────────────────────

"""
    nero_diagnostics(nero::NeroOrchestrator) -> String

One-line NERO state summary for the diagnostic loop.
"""
function nero_diagnostics(nero::NeroOrchestrator)::String
    lobe_strs = String[]
    for i in 1:N_LOBES
        push!(lobe_strs, @sprintf("%s=%.2f", LOBE_NAMES[i], nero.relevance[i]))
    end
    dominant = argmax(nero.relevance)
    @sprintf("[NERO tick=%d] %s | dominant=%s | surprise=[%.3f,%.3f,%.3f,%.3f] | agg=causal",
        nero.tick_count,
        join(lobe_strs, " "),
        LOBE_NAMES[dominant],
        nero.surprise[1], nero.surprise[2], nero.surprise[3], nero.surprise[4])
end

println("[nero] nero_orchestrator.jl loaded — NERO graph ($(N_LOBES) lobes) + causal aggregation")
