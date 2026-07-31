# market_sde.jl — Geometric Brownian Motion Surprise Z-Score
#
# Computes how much the current price deviates from the GBM expectation.
# Under GBM: dS/S = μ dt + σ dW
#
# The surprise z-score measures:
#   Z = (observed_return - expected_drift) / realized_volatility
#
# High |Z| → price is moving abnormally relative to recent behavior
#   Z > 0  → price surging above GBM expectation (bullish anomaly)
#   Z < 0  → price crashing below GBM expectation (bearish anomaly)
#   Z ≈ 0  → price behaving as expected (normal regime)
#
# Integration with Spikenaut Brain:
#   - Anomaly detection: feed as continuous input to MarketEncoder
#   - Risk filter: high |Z| → increase inhibition (market is "surprising")
#   - Kelly adjustment: reduce position size when |Z| > 2 (extreme move)

using Statistics

# ─── GBM Parameters ───────────────────────────────────────────────────────────

"""
    GBMParams

Configuration for the GBM surprise estimator.

Fields:
  lookback      — number of past returns for drift/vol estimation
  annualization — scaling factor for annualized metrics (252 for daily, 365*24 for hourly)
"""
struct GBMParams
    lookback::Int
    annualization::Float64
end

function GBMParams(;
    lookback::Int=64,
    annualization::Float64=365.0 * 24.0)  # hourly by default
    GBMParams(lookback, annualization)
end

# ─── Core GBM Surprise ────────────────────────────────────────────────────────

"""
    gbm_surprise(prices, params) -> Float64

Compute the GBM surprise z-score for the most recent price relative to
the recent history.

# Arguments
- `prices::AbstractVector{Float64}`: price series (must have ≥ 3 elements)
- `params::GBMParams`: estimation parameters

# Returns
- Z-score ∈ [-∞, +∞] (typically [-3, 3] for normal markets)
  Clamped to [-5, 5] for numerical safety.

# Example
```julia
prices = [100.0, 101.0, 99.5, 102.0, 103.5]
z = gbm_surprise(prices)
```
"""
function gbm_surprise(prices::AbstractVector{Float64}, params::GBMParams)
    n = length(prices)
    if n < 3
        return 0.0
    end

    # Compute log returns
    lookback = min(params.lookback, n - 1)
    returns = log.(prices[end-lookback+1:end] ./ prices[end-lookback:end-1])

    # Estimate drift (μ) and volatility (σ) from recent returns
    μ = mean(returns)
    σ = std(returns; mean=μ, corrected=true)

    if σ < 1e-10
        return 0.0  # no volatility → no surprise
    end

    # The most recent observed return
    last_return = returns[end]

    # GBM expected return for one step: μ (drift component)
    # Surprise = (observed - expected) / volatility
    Z = (last_return - μ) / σ

    return clamp(Z, -5.0, 5.0)
end

"""
    gbm_surprise_multi(prices, params) -> (Z, μ, σ)

Returns the z-score along with the estimated drift and volatility.
Useful for downstream components that need the raw parameters.
"""
function gbm_surprise_multi(prices::AbstractVector{Float64}, params::GBMParams)
    n = length(prices)
    if n < 3
        return (0.0, 0.0, 0.0)
    end

    lookback = min(params.lookback, n - 1)
    returns = log.(prices[end-lookback+1:end] ./ prices[end-lookback:end-1])

    μ = mean(returns)
    σ = std(returns; mean=μ, corrected=true)

    if σ < 1e-10
        return (0.0, μ, 0.0)
    end

    last_return = returns[end]
    Z = clamp((last_return - μ) / σ, -5.0, 5.0)

    return (Z, μ, σ)
end

# ─── Rolling GBM Surprise for Streaming Data ──────────────────────────────────

"""
    RollingGBM(window_size; params)

Streaming GBM surprise estimator using a rolling price window.
Call `update!` each tick to get the current anomaly z-score.

Fields:
  prices      — rolling price buffer (circular)
  window_size — total buffer size
  params      — GBM estimation parameters
  pos         — current write position
  count       — total ticks seen
"""
mutable struct RollingGBM
    prices::Vector{Float64}
    window_size::Int
    params::GBMParams
    pos::Int
    count::Int
end

"""
    RollingGBM(window_size=128; params) -> RollingGBM

Create a streaming GBM surprise estimator.
"""
function RollingGBM(window_size::Int=128;
    params::GBMParams=GBMParams(lookback=64))
    RollingGBM(zeros(Float64, window_size), window_size, params, 1, 0)
end

"""
    update!(rg, price) -> Float64

Process a new price tick. Returns the current GBM surprise z-score.
"""
function update!(rg::RollingGBM, price::Float64)
    rg.prices[rg.pos] = price
    rg.count += 1
    rg.pos = rg.pos >= rg.window_size ? 1 : rg.pos + 1

    if rg.count < 3
        return 0.0
    end

    # Extract the rolling window (only valid data)
    if rg.count >= rg.window_size
        window = rg.prices
    else
        window = @view rg.prices[1:rg.count]
    end

    return gbm_surprise(window, rg.params)
end

# ─── Normalized Surprise for SNN Input ────────────────────────────────────────

"""
    normalized_surprise(Z) -> Float64

Normalize the z-score to [0, 1] for SNN input encoding.
Maps Z from [-3, 3] to [0, 1], with Z=0 → 0.5 (neutral).
"""
function normalized_surprise(Z::Float64)
    return clamp((Z + 3.0) / 6.0, 0.0, 1.0)
end

"""
    surprise_magnitude(Z) -> Float64

Returns |Z| normalized to [0, 1]. Useful for risk filtering —
high magnitude means "something unusual is happening."
"""
function surprise_magnitude(Z::Float64)
    return clamp(abs(Z) / 3.0, 0.0, 1.0)
end

# ─── Multi-Asset GBM Surprise ─────────────────────────────────────────────────

"""
    compute_gbm_surprises(assets::Dict{String, Vector{Float64}}; params) -> Dict{String, Float64}

Compute GBM surprise z-scores for multiple assets simultaneously.
Returns a dictionary mapping ticker → z-score.

# Example
```julia
assets = Dict(
    "BTC" => [65000.0, 65200.0, 64800.0, 66000.0],
    "ETH" => [3500.0, 3480.0, 3520.0, 3450.0]
)
surprises = compute_gbm_surprises(assets)
```
"""
function compute_gbm_surprises(assets::Dict{String, <:AbstractVector{Float64}};
    params::GBMParams=GBMParams())
    return Dict(ticker => gbm_surprise(prices, params) for (ticker, prices) in assets)
end

println("[math] market_sde.jl loaded — GBM surprise estimator ready")
