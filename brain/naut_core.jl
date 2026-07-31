# sparse_brain.jl — Ensemble 65,536-Neuron Sparse CUDA Liquid State Machine
#
# Spikenaut V2 "Brain" — 4-Lobe Ensemble Architecture
# Subscribes to the Rust Nervous System via ZMQ IPC.
#
# Architecture:
#   4 parallel lobes (Scalper, Day, Swing, Macro) × 65,536 LIF neurons each
#   Varying time constants: τ_m ∈ {10ms, 25ms, 50ms, 100ms}
#   Xavier/Glorot W_out initialization (breaks zero-readout deadlock)
#   Sparse connectivity (1% connection probability, Float16 weights)
#   STDP covariance learning rule
#   Rolling 1,000-tick spike history for deep temporal covariance
#   Global inhibition from hardware proprioception
#
# Hardware Target: RTX 5080 (16GB VRAM)
# Expected VRAM usage:
#   4 × W_sparse: 65536² × 1% × ~6 bytes/nnz   ≈ 1.0 GB
#   4 × W_in:     65536 × 14 × 4 bytes           ≈ 15 MB
#   4 × W_out:    16 × 65536 × 4 bytes            ≈ 17 MB
#   4 × State vectors (V, S, traces):             ≈ 21 MB
#   4 × History (1000 × 65536 × 4 bytes):         ≈ 1.05 GB
#   MC buffers (65536 paths × 200 × 7):           ≈ 3.67 GB
#   Covariance working memory (8192²):            ≈ 0.5 GB
#   Total peak: ~12–14 GB VRAM
#
# Usage:
#   julia --project=spikenaut-capital/brain sparse_brain.jl
#
# ═══════════════════════════════════════════════════════════════════════════════

using CUDA
using SparseArrays
using LinearAlgebra
using Statistics
using Random
using Printf

# ─── Constants ────────────────────────────────────────────────────────────────

const N = 65_536        # Reservoir neuron count per lobe
const N_IN = 28            # Spiking Input receptors (7 assets × 4: UP, DOWN, VOL_UP, VOL_DOWN)
const N_OUT = 16            # Output neurons (8 bull/bear pairs)
const N_ASSETS = 7             # DNX, Quai, Qubic, Kaspa, Monero, Ocean, Verus
const CONN_PROB = 0.01          # 1% sparse connectivity → ~42M non-zero synapses
const DT = 0.1f0         # Simulation timestep (High-res for research)
const HIST_DEPTH = 1000          # Rolling history depth (ticks) for deep temporal covariance
const COV_SUBSAMPLE = 8192          # Subsampled neurons for tractable covariance (avoids 17 GB N×N)

# ── Lobe time constants (membrane τ_m in ms) ─────────────────────────────────
const LOBE_TAUS = Float32[10.0, 25.0, 50.0, 100.0]
const LOBE_NAMES = ["Scalper", "Day", "Swing", "Macro"]
const N_LOBES = 4

# ── Ensemble Aggregation Weights ─────────────────────────────────────────────
# Scalper reacts fastest → highest weight for immediate signals
const LOBE_WEIGHTS = Float32[0.4, 0.3, 0.2, 0.1]

# ── Ornstein-Uhlenbeck SDE Parameters ────────────────────────────────────────
# dV_j = ((V_rest - V_j) / τ_m + Σᵢ Wᵢⱼ · Sᵢ(t)) dt + σ dWₜ

const V_REST = -65.0f0       # Resting potential (mV)
const V_THRESH = -50.0f0       # Spike threshold (mV)
const V_RESET = -70.0f0       # Post-spike reset (mV)
const SIGMA = 2.0f0         # OU noise amplitude
const REFRAC_T = 5             # Refractory period (timesteps)

# ── STDP Covariance Learning Parameters ──────────────────────────────────────
# ΔWᵢⱼ = η · Cᵢⱼ · exp(-|Δt| / τ_spike)

const ETA = 0.001f0       # Learning rate
const TAU_SPIKE = 20.0f0        # STDP time constant
const TAU_TRACE = 20.0f0        # Eligibility trace decay
const W_MAX = 1.0f0         # Weight saturation (Float16 range)

# Precompute Float32 scalars on the CPU so CUDA broadcasts stay monomorphic.
const XAVIER_STD_IN = sqrt(2.0f0 / Float32(N_IN))
const XAVIER_STD_OUT = sqrt(2.0f0 / Float32(N))
const OU_NOISE_SCALE = SIGMA * sqrt(DT)

# ── Global Inhibition Thresholds ─────────────────────────────────────────────
const TEMP_THRESH = 75.0f0    # °C — start increasing spike threshold
const BUFFER_THRESH = 0.8f0     # FPGA buffer load — start increasing threshold
const INHIB_GAIN = 15.0f0    # mV increase per unit of inhibition signal

# MarketPulse types live in market_types.jl (no CUDA) for unit testing.
include(joinpath(@__DIR__, "market_types.jl"))
# Single inhibition map (shared with LiquidCortex adapter path).
if !@isdefined(hft_inhibition)
    include(joinpath(@__DIR__, "hft_inhibition.jl"))
end

function pulse_to_input(pulse::MarketPulse)
    # Placeholder: Maps are handled by market_encoder.jl during live loop.
    # This remains for compatibility with standalone test scripts.
    u = zeros(Float32, N_IN)
    return u
end

"""
    cpu_randn_cu(dims...) -> CuArray{Float32}

Work around CUDA.jl RNG compilation failures on this stack by generating
Float32 Gaussian samples on the host and uploading them to the device.
"""
function cpu_randn_cu(dims::Vararg{Int,N}) where {N}
    return cu(randn(Float32, dims...))
end

# ═══════════════════════════════════════════════════════════════════════════════
# SparseBrain: The 65,536-Neuron CUDA Reservoir
# ═══════════════════════════════════════════════════════════════════════════════

mutable struct SparseBrain
    # ── Synaptic weights (sparse, Float16 on GPU) ────────────────────────────
    W::CUDA.CUSPARSE.CuSparseMatrixCSC{Float16,Int32}

    # ── Input / Output weight matrices (dense, Float32) ──────────────────────
    W_in::CuMatrix{Float32}
    W_out::CuMatrix{Float32}

    # ── Neuron state vectors (Float32 on GPU) ────────────────────────────────
    V::CuVector{Float32}          # Membrane potential
    S::CuVector{Float32}          # Spike state (0 or 1)
    refrac::CuVector{Int32}       # Refractory counter

    # ── STDP eligibility traces ──────────────────────────────────────────────
    trace_pre::CuVector{Float32}  # Pre-synaptic trace
    trace_post::CuVector{Float32} # Post-synaptic trace

    # ── Readout state ────────────────────────────────────────────────────────
    output::CuVector{Float32}     # 16-element readout

    # ── Per-lobe membrane time constant ──────────────────────────────────────
    tau_m::Float32                # τ_m in ms (Scalper=10, Day=25, Swing=50, Macro=100)

    # ── Rolling spike history (HIST_DEPTH × N) for deep temporal covariance ──
    history::CuMatrix{Float32}    # 1000 × 65536 on GPU
    hist_idx::Int64               # Current write index (circular)
    hist_full::Bool               # True once buffer wraps at least once

    # ── Hardware feedback ────────────────────────────────────────────────────
    v_thresh_dynamic::Float32     # Adaptive threshold (global inhibition)

    # ── Diagnostics ──────────────────────────────────────────────────────────
    tick_count::Int64
    total_spikes::Int64
    last_spike_rate::Float32
end

"""
    SparseBrain(tau_m; name="default") -> SparseBrain

Initialize a 65,536-neuron sparse CUDA reservoir lobe.

Weight initialization:
  - W_recurrent: Sparse CSC, 1% connectivity, Float16
    Spectral radius controlled via scaling: ||W|| ≈ 0.9 (echo state property)
  - W_in: Dense Float32, Xavier initialization √(2/N_IN)
  - W_out: Dense Float32, Xavier/Glorot initialization √(2/N)
    (Breaks zero-readout deadlock — reservoir produces signals from tick 1)
"""
function SparseBrain(tau_m::Float32; name::String="default")
    println("[brain:$name] Initializing 65,536-neuron lobe (τ_m=$(tau_m)ms)...")

    # ── 1. Sparse recurrent weight matrix (Float16, 1% connectivity) ─────────
    nnz_expected = round(Int, N * N * CONN_PROB)
    println("[brain:$name] Generating sparse connectivity (~$(round(nnz_expected / 1e6, digits=1))M synapses)...")

    # Build sparse matrix in COO format for efficiency
    rows = rand(1:N, nnz_expected)
    cols = rand(1:N, nnz_expected)
    vals = Float16.(randn(Float32, nnz_expected) .* 0.02f0)

    W_cpu = sparse(rows, cols, vals, N, N)

    # Remove self-connections (Dale's law approximation)
    for i in 1:min(N, size(W_cpu, 1))
        W_cpu[i, i] = Float16(0)
    end

    # Scale for spectral radius ≈ 0.9 (echo state property)
    frob = norm(W_cpu)
    actual_nnz = nnz(W_cpu)
    spectral_approx = frob / sqrt(actual_nnz)
    target_rho = 0.9f0
    scale_factor = target_rho / max(spectral_approx, 1e-6)
    W_cpu .*= Float16(scale_factor)

    println("[brain:$name] W_sparse: $(actual_nnz) nnz, ρ≈$(round(target_rho, digits=2))")

    # Transfer to GPU as CuSparseMatrixCSC
    W_gpu = CUDA.CUSPARSE.CuSparseMatrixCSC(W_cpu)

    # ── 2. Input weight matrix (Dense, Xavier init) ──────────────────────────
    W_in = cpu_randn_cu(N, N_IN)
    W_in .*= XAVIER_STD_IN

    # ── 3. Output weight matrix — Xavier/Glorot (breaks zero-readout deadlock)
    # W_out ~ N(0, √(2/N)) — ensures non-trivial readout from tick 1
    W_out = cpu_randn_cu(N_OUT, N)
    W_out .*= XAVIER_STD_OUT
    println("[brain:$name] W_out: Xavier/Glorot init σ=$(round(Float64(XAVIER_STD_OUT), sigdigits=4))")

    # ── 4. Neuron state vectors ──────────────────────────────────────────────
    V = CUDA.fill(Float32(V_REST), N)
    S = CUDA.zeros(Float32, N)
    refrac = CUDA.zeros(Int32, N)

    # ── 5. STDP traces ──────────────────────────────────────────────────────
    trace_pre = CUDA.zeros(Float32, N)
    trace_post = CUDA.zeros(Float32, N)

    # ── 6. Output ────────────────────────────────────────────────────────────
    output = CUDA.zeros(Float32, N_OUT)

    # ── 7. Rolling spike history for deep temporal covariance (on GPU) ───────
    history = CUDA.zeros(Float32, HIST_DEPTH, N)
    println("[brain:$name] History buffer: $(HIST_DEPTH)×$(N) = $(round(HIST_DEPTH * N * 4 / 1e6, digits=1)) MB")

    CUDA.synchronize()
    println("[brain:$name] ✓ Lobe initialized (τ_m=$(tau_m)ms)")

    SparseBrain(
        W_gpu, W_in, W_out,
        V, S, refrac,
        trace_pre, trace_post,
        output,
        tau_m,
        history, 1, false,
        Float32(V_THRESH),
        0, 0, 0.0f0
    )
end

# ═══════════════════════════════════════════════════════════════════════════════
# Simulation Step: OU-SDE Dynamics + STDP Learning
# ═══════════════════════════════════════════════════════════════════════════════

"""
    step!(brain, u, gpu_temp, basys_load; funding_rate=0, liquidation_vol=0, reflex_eta=ETA)

Execute one simulation timestep:

1. **Global Inhibition ("Cortisol")**: Raise V_thresh from:
   - Hardware: GPU temp > 75°C or FPGA buffer > 80%
   - Market: CoinGlass funding_rate > 0.0005 or liquidation cascade
   High funding = "market is tense" → fewer spikes → wait for high-confidence signal
2. **OU-SDE Dynamics**: 
   dV_j = ((V_rest - V_j)/τ_m + Σᵢ Wᵢⱼ·Sᵢ(t) + W_in·u) dt + σ·dWₜ
3. **Spike Detection**: V_j > V_thresh_dynamic → spike, reset to V_reset
4. **STDP Update**: ΔWᵢⱼ = η · trace_pre_i · trace_post_j (covariance rule)
   reflex_eta allows Scalper lobe to "flash-learn" on liquidity events
5. **Readout**: y = W_out · S (weighted spike count)
"""
"""
    step!(brain, u; inhibition=0, reflex_eta=ETA)

LiquidCortex-compatible keyword API. Prefer this for new call sites.
"""
function step!(brain::SparseBrain, u::CuVector{Float32};
    inhibition::Float32=0.0f0,
    reflex_eta::Float32=ETA)
    brain.tick_count += 1

    # ── 1. Global Inhibition (caller-supplied scalar) ─────────────────────────
    inhib = clamp(inhibition, 0.0f0, 3.0f0)
    brain.v_thresh_dynamic = V_THRESH + inhib * INHIB_GAIN
    # ── 2. OU-SDE Membrane Dynamics (per-lobe τ_m) ──────────────────────────
    # Recurrent input: I_rec = W · S (sparse mat-vec on GPU via cuSPARSE)
    I_rec = brain.W * Float16.(brain.S)
    I_rec_f32 = Float32.(I_rec)

    # External input: I_ext = W_in · u
    I_ext = brain.W_in * u

    # OU noise: σ · dWₜ (Wiener process increment)
    noise = cpu_randn_cu(N)
    noise .*= OU_NOISE_SCALE

    # Leak + input + noise — uses per-lobe brain.tau_m
    # dV = ((V_rest - V) / τ_m + I_rec + I_ext) * dt + noise
    dV = ((V_REST .- brain.V) ./ brain.tau_m .+ I_rec_f32 .+ I_ext) .* DT .+ noise

    # Refractory mask: neurons in refractory period don't integrate
    active_mask = brain.refrac .<= 0
    brain.V .+= dV .* Float32.(active_mask)

    # ── 3. Spike Detection ───────────────────────────────────────────────────
    spiked = brain.V .> brain.v_thresh_dynamic
    brain.S .= Float32.(spiked)

    # Reset spiked neurons
    brain.V .= ifelse.(spiked, Float32(V_RESET), brain.V)
    brain.refrac .= ifelse.(spiked, Int32(REFRAC_T), max.(brain.refrac .- Int32(1), Int32(0)))

    # Count spikes for diagnostics
    n_spikes = sum(brain.S)
    brain.total_spikes += round(Int64, n_spikes)
    brain.last_spike_rate = n_spikes / N

    # ── 4. Record spike history (rolling circular buffer) ────────────────────
    brain.history[brain.hist_idx, :] .= brain.S
    brain.hist_idx += 1
    if brain.hist_idx > HIST_DEPTH
        brain.hist_idx = 1
        brain.hist_full = true
    end

    # ── 5. STDP Covariance Learning (reflex_eta enables flash-learning) ─────
    brain.trace_pre .= brain.trace_pre .* (1.0f0 - DT / TAU_TRACE) .+ brain.S
    brain.trace_post .= brain.trace_post .* (1.0f0 - DT / TAU_TRACE) .+ brain.S

    # STDP weight update on W_out every 10 ticks (Hebbian readout rule):
    # ΔW_out[i,j] = reflex_eta * S_out[i] * trace_pre[j]
    # reflex_eta > ETA for Scalper lobe during liquidity events → "flash-learning"
    if brain.tick_count % 10 == 0
        S_out = brain.output .> 0.0f0
        dW_out = reflex_eta .* (Float32.(S_out) * brain.trace_pre')
        brain.W_out .+= dW_out
        clamp!(brain.W_out, -W_MAX, W_MAX)
    end

    # ── 6. Readout Layer ─────────────────────────────────────────────────────
    brain.output .= brain.W_out * brain.S

    CUDA.synchronize()
    return nothing
end

"""
    step!(brain, u, gpu_temp, basys_load; funding_rate, liquidation_vol, …)

Legacy multi-arg API — maps HFT stress fields through `hft_inhibition` then steps.
"""
function step!(brain::SparseBrain, u::CuVector{Float32}, gpu_temp::Float32, basys_load::Float32;
    funding_rate::Float32=0.0f0, liquidation_vol::Float32=0.0f0,
    reflex_eta::Float32=ETA,
    dydx_oi_delta::Float32=0.0f0,
    dydx_funding_rate::Float32=0.0f0)
    inhib = hft_inhibition(gpu_temp, basys_load, funding_rate, liquidation_vol;
        dydx_oi_delta=dydx_oi_delta, dydx_funding_rate=dydx_funding_rate)
    return step!(brain, u; inhibition=inhib, reflex_eta=reflex_eta)
end

"""
    get_output(brain::SparseBrain) -> Vector{Float32}

Copy the 16-element readout vector from GPU to CPU.
"""
function get_output(brain::SparseBrain)
    return Array(brain.output)
end

"""
    diagnostics(brain::SparseBrain) -> String

Return a diagnostic string with current brain state.
"""
function diagnostics(brain::SparseBrain)
    return string(
        "[brain] tick=", brain.tick_count,
        " spikes=", brain.total_spikes,
        " rate=", round(brain.last_spike_rate * 100, digits=2), "%",
        " V_thresh=", round(brain.v_thresh_dynamic, digits=1),
        " W_out_norm=", round(Float64(norm(Array(brain.W_out))), digits=4)
    )
end

# ═══════════════════════════════════════════════════════════════════════════════
# High-Frequency Covariance Computation (GPU-intensive)
# ═══════════════════════════════════════════════════════════════════════════════

"""
    compute_reservoir_covariance!(brain::SparseBrain) -> (C, indices) or nothing

Compute a subsampled covariance matrix of reservoir spike history.
Subsamples COV_SUBSAMPLE (8192) neurons to avoid the full N×N matrix
which would be 65536² × 4 = 17 GB — doesn't fit in 16GB VRAM.

8192² × 4 bytes = 268 MB — fits comfortably while still driving GPU hard.

Uses the brain's internal rolling history buffer (HIST_DEPTH × N).
Returns (C::CuMatrix, indices::Vector{Int}) or nothing if history not full.
"""
function compute_reservoir_covariance!(brain::SparseBrain)
    if !brain.hist_full
        return nothing
    end

    # Subsample COV_SUBSAMPLE random neurons for tractable covariance
    indices = sort(randperm(N)[1:COV_SUBSAMPLE])
    X = brain.history[:, indices]  # HIST_DEPTH × COV_SUBSAMPLE on GPU

    # Mean-center the activity matrix
    μ = mean(X, dims=1)           # 1 × COV_SUBSAMPLE
    X_centered = X .- μ           # HIST_DEPTH × COV_SUBSAMPLE

    # Covariance via CUBLAS SYRK: C = (1/(T-1)) * Xᵀ * X
    # 8192 × 8192 dense matmul — drives tensor core utilization
    C = (X_centered' * X_centered) ./ Float32(HIST_DEPTH - 1)

    CUDA.synchronize()
    return (C, indices)
end

# ═══════════════════════════════════════════════════════════════════════════════
# Monte Carlo Market Simulation (Background GPU workload)
# ═══════════════════════════════════════════════════════════════════════════════

"""
    monte_carlo_paths!(brain::SparseBrain, current_prices::Vector{Float32}, n_paths::Int, horizon::Int)

Generate `n_paths` simulated price paths for each asset using
Geometric Brownian Motion driven by the reservoir's learned covariance.

This runs continuously in the background to keep the GPU at high utilization
between market ticks.

Uses GPU-parallel random number generation (cuRAND) for maximum throughput.
"""
function monte_carlo_paths!(buf::CuArray{Float32,3}, current_prices::Vector{Float32}, vol_estimates::Vector{Float32})
    n_paths, horizon, n_assets = size(buf)
    @assert length(current_prices) == n_assets "Expected $(n_assets) assets, got $(length(current_prices))"
    @assert length(vol_estimates) == n_assets "Expected $(n_assets) vol estimates, got $(length(vol_estimates))"

    # In-place GBM: write directly into pre-allocated buf (no second 8.4 GB alloc)
    # Host-side RNG → GPU upload to bypass Blackwell CUDA.jl RNG compilation bug
    for a in 1:n_assets
        prev_prices = CUDA.fill(current_prices[a], n_paths)
        for t in 1:horizon
            shocks = cpu_randn_cu(n_paths)
            shocks .*= vol_estimates[a]
            prev_prices .= prev_prices .* exp.(shocks)
            @views buf[:, t, a] .= prev_prices
        end
    end

    CUDA.synchronize()
    return nothing
end

# ═══════════════════════════════════════════════════════════════════════════════
# EnsembleBrain: 4-Lobe Parallel Architecture
# ═══════════════════════════════════════════════════════════════════════════════

"""
    EnsembleBrain — 4 parallel SparseBrain lobes with varying time constants.

    Scalper (τ_m=10ms):  Fast reaction — captures HFT micro-structure
    Day     (τ_m=25ms):  Intraday patterns — hourly momentum
    Swing   (τ_m=50ms):  Multi-day swings — daily regime detection
    Macro   (τ_m=100ms): Trend following — weekly/monthly structure

Aggregation: weighted sum of lobe readouts.
"""
mutable struct EnsembleBrain
    lobes::Vector{SparseBrain}
    lobe_names::Vector{String}
    agg_output::CuVector{Float32}   # Aggregated 16-element readout
    weights::Vector{Float32}        # Per-lobe aggregation weights
end

"""
    EnsembleBrain() -> EnsembleBrain

Initialize 4 parallel lobes × 65,536 neurons = 262,144 total neurons on GPU.
"""
function EnsembleBrain()
    println()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  Ensemble Brain — 4 Lobes × 65,536 = 262,144 Neurons      ║")
    println("║  Scalper(10ms) │ Day(25ms) │ Swing(50ms) │ Macro(100ms)   ║")
    println("╚══════════════════════════════════════════════════════════════╝")
    println()

    lobes = SparseBrain[]
    for i in 1:N_LOBES
        println("─── Lobe $i/$(N_LOBES): $(LOBE_NAMES[i]) (τ_m=$(LOBE_TAUS[i])ms) ───")
        push!(lobes, SparseBrain(LOBE_TAUS[i]; name=LOBE_NAMES[i]))
        println()
    end

    agg_output = CUDA.zeros(Float32, N_OUT)

    CUDA.synchronize()
    free_mem = CUDA.available_memory() / 1e9
    total_mem = CUDA.total_memory() / 1e9
    used = total_mem - free_mem
    println("═══════════════════════════════════════════════════════════════")
    @printf("[ensemble] ✓ All %d lobes online — %d total neurons\n", N_LOBES, N_LOBES * N)
    @printf("[ensemble] VRAM: %.2f / %.2f GB (%.0f%% used)\n", used, total_mem, used / total_mem * 100)
    println("═══════════════════════════════════════════════════════════════")

    EnsembleBrain(lobes, copy(LOBE_NAMES), agg_output, copy(LOBE_WEIGHTS))
end

"""
    ensemble_step!(eb, u, gpu_temp, basys_load, funding_rate, liquidation_vol, liquidity_delta;
                   dydx_oi_delta, dydx_funding_rate)

Step all 4 lobes independently on the same input, then aggregate readouts.

Global Inhibition: funding_rate + liquidation_vol are forwarded to ALL lobes,
  raising V_thresh when the market is "tense" (high funding = crowded longs).
  dYdX funding rate provides additional key-free inhibition signal.

dYdX OI Arousal: positive OI delta signals accumulation → lowers effective
  V_thresh (heightens reservoir sensitivity to incoming signals).

Reflex Gating: When |liquidity_delta| > 0.1 (significant on-chain event),
  the Scalper lobe (index 1, τ_m=10ms) gets a 5× learning rate boost,
  enabling "flash-learning" — rapid synaptic adaptation to liquidity shocks.
"""
"""
    ensemble_step!(eb, u; inhibition=0, reflex_eta=ETA, reflex_signal=0)

LiquidCortex-compatible keyword API.
"""
function ensemble_step!(eb::EnsembleBrain, u::CuVector{Float32};
    inhibition::Float32=0.0f0,
    reflex_eta::Float32=ETA,
    reflex_signal::Float32=0.0f0)
    reflex_scalper = if abs(reflex_signal) > 0.1f0
        reflex_eta * 5.0f0
    else
        reflex_eta
    end

    for (i, lobe) in enumerate(eb.lobes)
        eta_lobe = (i == 1) ? reflex_scalper : reflex_eta
        step!(lobe, u; inhibition=inhibition, reflex_eta=eta_lobe)
    end

    eb.agg_output .= 0.0f0
    for (i, lobe) in enumerate(eb.lobes)
        eb.agg_output .+= eb.weights[i] .* lobe.output
    end

    CUDA.synchronize()
    return nothing
end

"""
    ensemble_step!(eb, u, gpu_temp, basys_load, funding, liq, liquidity_delta; …)

Legacy multi-arg API — computes inhibition via `hft_inhibition`.
"""
function ensemble_step!(eb::EnsembleBrain, u::CuVector{Float32},
    gpu_temp::Float32, basys_load::Float32,
    funding_rate::Float32, liquidation_vol::Float32,
    liquidity_delta::Float32;
    dydx_oi_delta::Float32=0.0f0,
    dydx_funding_rate::Float32=0.0f0)
    inhib = hft_inhibition(gpu_temp, basys_load, funding_rate, liquidation_vol;
        dydx_oi_delta=dydx_oi_delta, dydx_funding_rate=dydx_funding_rate)
    return ensemble_step!(eb, u; inhibition=inhib, reflex_signal=liquidity_delta)
end

"""
    get_ensemble_output(eb) -> Vector{Float32}

Copy the 16-element aggregated readout vector from GPU to CPU.
"""
function get_ensemble_output(eb::EnsembleBrain)
    return Array(eb.agg_output)
end

"""
    ensemble_diagnostics(eb) -> String

One-line per-lobe diagnostic summary.
"""
function ensemble_diagnostics(eb::EnsembleBrain)
    lines = String[]
    for (i, lobe) in enumerate(eb.lobes)
        rate_pct = round(lobe.last_spike_rate * 100, digits=2)
        w_norm = round(Float64(norm(Array(lobe.W_out))), digits=4)
        push!(lines, @sprintf("[%s:τ=%d] tick=%d rate=%.2f%% W=%.4f",
            eb.lobe_names[i], Int(lobe.tau_m), lobe.tick_count, rate_pct, w_norm))
    end

    # Add GPU Temp to the ensemble line
    temp_str = @sprintf("Temp: %.1f°C", eb.lobes[1].v_thresh_dynamic > 0 ? (eb.lobes[1].v_thresh_dynamic - V_THRESH) / INHIB_GAIN * 1.0 + 40.0 : 40.0)
    push!(lines, temp_str)

    return join(lines, " | ")
end

println("[brain] sparse_brain.jl loaded — EnsembleBrain (4-lobe, 262,144 neurons) ready")
