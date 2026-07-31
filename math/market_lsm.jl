# market_lsm.jl — Lightweight Signal Preprocessor (Fast Reflex Layer)
#
# A small 256-neuron liquid state machine that sits between the MarketEncoder
# and the main EnsembleBrain. Its job is rapid feature extraction:
#
#   MarketEncoder (28 spikes) → [FastReflex LSM] → (16 features) → EnsembleBrain
#
# Unlike the main 65K-neuron sparse reservoirs in naut_core.jl, this is:
#   - Dense (not sparse) for fast GPU throughput on small matrices
#   - Small (256 neurons) for minimal latency
#   - Stateless per-tick (no STDP learning) for deterministic behavior
#   - Pure feature extractor (no decision-making)
#
# The fast reflex layer detects micro-patterns in the spike input that the
# larger reservoirs might miss due to their longer time constants:
#   - Rapid price direction changes (sub-tick)
#   - Volatility spikes (sudden regime shifts)
#   - Input correlation patterns (multi-asset co-movement)
#
# Hardware Target: RTX 5080 (runs alongside the main brain, <1ms added latency)
# Memory: 256×256×4B + 256×28×4B + 256×16×4B ≈ 300 KB (negligible)

using CUDA
using LinearAlgebra

# ─── Fast Reflex LSM Parameters ───────────────────────────────────────────────

const REFLEX_N = 256         # Reservoir neurons (small, fast)
const REFLEX_N_IN = 28       # Input: matches MarketEncoder output (7 assets × 4 channels)
const REFLEX_N_OUT = 16      # Output: features for EnsembleBrain
const REFLEX_TAU = 0.5f0     # Membrane time constant (fast dynamics)
const REFLEX_LEAK = 0.1f0    # Leak rate (high = fast adaptation)

# ─── Fast Reflex State ────────────────────────────────────────────────────────

"""
    FastReflex

Lightweight signal preprocessor using a small dense LSM.
Stateless per-tick — no learning, no history buffer.

Fields:
  W     — recurrent weight matrix (REFLEX_N × REFLEX_N, Float32)
  W_in  — input weight matrix (REFLEX_N × REFLEX_N_IN, Float32)
  W_out — readout weight matrix (REFLEX_N_OUT × REFLEX_N, Float32)
  x     — reservoir state (REFLEX_N, Float32)
"""
mutable struct FastReflex
    W::CuMatrix{Float32}
    W_in::CuMatrix{Float32}
    W_out::CuMatrix{Float32}
    x::CuVector{Float32}
end

"""
    FastReflex() -> FastReflex

Initialize the fast reflex preprocessor.
Weight initialization: Xavier/Glorot for all matrices.
"""
function FastReflex()
    println("[math:lsm] Initializing Fast Reflex LSM ($(REFLEX_N) neurons)...")

    # Recurrent weights: scaled for echo state property (spectral radius ≈ 0.9)
    W_cpu = randn(Float32, REFLEX_N, REFLEX_N) .* 0.02f0
    # Spectral radius approximation: scale by 1/sqrt(REFLEX_N)
    W_cpu ./= sqrt(Float32(REFLEX_N))
    W_cpu .*= 0.9f0

    W_gpu = cu(W_cpu)

    # Input weights: Xavier init
    W_in = cu(randn(Float32, REFLEX_N, REFLEX_N_IN) .* sqrt(2.0f0 / Float32(REFLEX_N_IN)))

    # Output weights: Xavier init (small output range)
    W_out = cu(randn(Float32, REFLEX_N_OUT, REFLEX_N) .* sqrt(2.0f0 / Float32(REFLEX_N)))

    # Reservoir state
    x = CUDA.zeros(Float32, REFLEX_N)

    println("[math:lsm] ✓ Fast Reflex ready — $(REFLEX_N * sizeof(Float32) ÷ 1024) KB on GPU")
    return FastReflex(W_gpu, W_in, W_out, x)
end

# ─── Core Step ────────────────────────────────────────────────────────────────

"""
    step!(reflex, u_gpu) -> CuVector{Float32}

Execute one timestep of the fast reflex reservoir.

# Arguments
- `reflex::FastReflex`: the preprocessor state
- `u_gpu::CuVector{Float32}`: 28-element spike input from MarketEncoder (on GPU)

# Returns
- 16-element feature vector (on GPU) for the EnsembleBrain

# Dynamics
    x(t+1) = (1 - leak) * x(t) + leak * tanh(W·x(t) + W_in·u(t))
    y(t)   = W_out · x(t+1)
"""
function step!(reflex::FastReflex, u_gpu::CuVector{Float32})
    # Reservoir dynamics: leaky integrator with tanh activation
    recurrent = reflex.W * reflex.x
    external = reflex.W_in * u_gpu

    # Leaky update: x = (1-α)*x + α*tanh(W*x + W_in*u)
    reflex.x .= (1.0f0 - REFLEX_LEAK) .* reflex.x .+
                REFLEX_LEAK .* tanh.(recurrent .+ external)

    # Readout
    y = reflex.W_out * reflex.x

    return y
end

"""
    step_cpu(reflex, u_cpu) -> Vector{Float32}

CPU convenience wrapper. Takes a CPU input vector, returns a CPU output vector.
Automatically handles GPU transfer.
"""
function step_cpu(reflex::FastReflex, u_cpu::Vector{Float32})
    u_gpu = cu(u_cpu)
    y_gpu = step!(reflex, u_gpu)
    return Array(y_gpu)
end

# ─── Batch Feature Extraction ─────────────────────────────────────────────────

"""
    extract_features(reflex, spike_series) -> Matrix{Float32}

Process a series of spike vectors and return extracted features.
Useful for offline analysis and backtesting.

# Arguments
- `reflex::FastReflex`: preprocessor state (will be mutated)
- `spike_series::Matrix{Float32}`: N_IN × T matrix (each column is one tick's spikes)

# Returns
- N_OUT × T matrix of extracted features
"""
function extract_features(reflex::FastReflex, spike_series::AbstractMatrix{Float32})
    n_in, T = size(spike_series)
    features = zeros(Float32, REFLEX_N_OUT, T)

    for t in 1:T
        u_gpu = cu(@view spike_series[:, t])
        y_gpu = step!(reflex, u_gpu)
        features[:, t] = Array(y_gpu)
    end

    return features
end

# ─── Reset ────────────────────────────────────────────────────────────────────

"""
    reset!(reflex)

Reset reservoir state to zeros. Use between sessions or backtests.
"""
function reset!(reflex::FastReflex)
    CUDA.fill!(reflex.x, 0.0f0)
    return nothing
end

println("[math] market_lsm.jl loaded — Fast Reflex LSM (256 neurons) ready")
