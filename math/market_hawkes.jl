# market_hawkes.jl — Hawkes Self-Exciting Point Process Intensity
#
# Estimates the conditional intensity λ(t) of price events using an
# exponential kernel Hawkes process. Useful for detecting self-excitation
# bursts — periods where price movements trigger further movements.
#
# The intensity at time t is:
#   λ(t) = μ + Σ_{t_i < t} α · exp(-β · (t - t_i))
#
# where:
#   μ   = base intensity (background rate)
#   α   = excitation magnitude (how much each event boosts intensity)
#   β   = decay rate (how fast the boost fades)
#   t_i = timestamps of past events
#
# High λ(t) → market is in a self-exciting regime (momentum/panic)
# Low  λ(t) → market is calm (mean-reverting / quiet)
#
# Integration with Spikenaut Brain:
#   - Feed as continuous input to MarketEncoder (spike probability scaling)
#   - High intensity → increase Scalper lobe sensitivity
#   - Use as volatility regime filter for Kelly sizing

using Statistics

# ─── Hawkes Parameters ────────────────────────────────────────────────────────

"""
    HawkesParams

Configuration for the Hawkes intensity estimator.

Fields:
  baseline_μ    — background event rate (events per timestep)
  excitation_α  — excitation magnitude per event
  decay_β       — exponential decay rate (higher = shorter memory)
  dt            — timestep for discrete approximation
"""
struct HawkesParams
    baseline_μ::Float64
    excitation_α::Float64
    decay_β::Float64
    dt::Float64
end

function HawkesParams(;
    baseline_μ::Float64=0.01,
    excitation_α::Float64=0.5,
    decay_β::Float64=0.1,
    dt::Float64=1.0)
    HawkesParams(baseline_μ, excitation_α, decay_β, dt)
end

# ─── Core Intensity Computation ───────────────────────────────────────────────

"""
    hawkes_intensity(events, t, params) -> Float64

Compute the Hawkes conditional intensity at time `t` given past events.

# Arguments
- `events::Vector{Float64}`: timestamps of past events (must be sorted ascending)
- `t::Float64`: current time
- `params::HawkesParams`: Hawkes parameters

# Returns
- λ(t): conditional intensity (≥ baseline_μ)

# Example
```julia
events = [1.0, 3.0, 5.0, 8.0]
λ = hawkes_intensity(events, 10.0, HawkesParams())
```
"""
function hawkes_intensity(events::AbstractVector{Float64}, t::Float64, params::HawkesParams)
    λ = params.baseline_μ

    for t_i in events
        if t_i >= t
            break  # events are sorted; no more past events
        end
        Δt = t - t_i
        λ += params.excitation_α * exp(-params.decay_β * Δt)
    end

    return λ
end

"""
    hawkes_intensity!(buffer, events, t, params) -> Float64

In-place version that writes intermediate λ values into `buffer`.
The last element of buffer is the final intensity at time `t`.
"""
function hawkes_intensity!(buffer::AbstractVector{Float64},
    events::AbstractVector{Float64},
    t::Float64,
    params::HawkesParams)
    λ = params.baseline_μ
    n = min(length(buffer), length(events))

    for i in 1:n
        Δt = t - events[i]
        if Δt < 0
            break
        end
        λ += params.excitation_α * exp(-params.decay_β * Δt)
        buffer[i] = λ
    end

    return λ
end

# ─── Rolling Hawkes for Streaming Data ────────────────────────────────────────

"""
    RollingHawkes(window_size, params)

Streaming Hawkes intensity estimator that maintains a rolling window
of price change events. Call `update!` each tick to get current intensity.

Fields:
  events     — circular buffer of event timestamps
  prices     — rolling price window for event detection
  params     — Hawkes parameters
  pos        — current write position in circular buffer
  count      — total events seen
  threshold  — minimum |Δprice| to register as an event
"""
mutable struct RollingHawkes
    events::Vector{Float64}
    prices::Vector{Float64}
    params::HawkesParams
    pos::Int
    count::Int
    threshold::Float64
    window_size::Int
end

"""
    RollingHawkes(window_size=100; params, threshold) -> RollingHawkes

Create a streaming Hawkes estimator.
"""
function RollingHawkes(window_size::Int=100;
    params::HawkesParams=HawkesParams(),
    threshold::Float64=0.001)
    RollingHawkes(
        zeros(Float64, window_size),
        zeros(Float64, window_size),
        params,
        1, 0, threshold, window_size)
end

"""
    update!(rh, price, timestamp) -> Float64

Process a new price tick. Returns the current Hawkes intensity λ(t).
An event is registered if |Δprice| > threshold.
"""
function update!(rh::RollingHawkes, price::Float64, timestamp::Float64)
    # Store price in circular buffer
    rh.prices[rh.pos] = price

    # Check for event (price jump exceeding threshold)
    if rh.count > 0
        prev_pos = rh.pos == 1 ? rh.window_size : rh.pos - 1
        Δprice = abs(price - rh.prices[prev_pos])

        if Δprice > rh.threshold
            # Register event
            rh.events[rh.pos] = timestamp
            rh.count += 1
        end
    else
        # First tick — initialize
        rh.events[rh.pos] = timestamp
        rh.count = 1
    end

    # Compute intensity using all events in window
    λ = rh.params.baseline_μ
    active_events = min(rh.count, rh.window_size)

    for i in 1:active_events
        Δt = timestamp - rh.events[i]
        if Δt >= 0
            λ += rh.params.excitation_α * exp(-rh.params.decay_β * Δt)
        end
    end

    # Advance circular buffer
    rh.pos = rh.pos >= rh.window_size ? 1 : rh.pos + 1

    return λ
end

# ─── Normalized Intensity for SNN Input ───────────────────────────────────────

"""
    normalized_intensity(λ, params) -> Float64

Normalize Hawkes intensity to [0, 1] for SNN input encoding.
Maps λ from [baseline, baseline + α/β] to [0, 1].
"""
function normalized_intensity(λ::Float64, params::HawkesParams)
    max_λ = params.baseline_μ + params.excitation_α / params.decay_β
    return clamp((λ - params.baseline_μ) / (max_λ - params.baseline_μ + 1e-8), 0.0, 1.0)
end

# ─── Batch Computation (for backtesting) ─────────────────────────────────────

"""
    compute_hawkes_series(prices; threshold, params) -> Vector{Float64}

Compute a rolling Hawkes intensity series from a price array.
Returns a vector of normalized intensities [0, 1] of the same length as prices.
"""
function compute_hawkes_series(prices::AbstractVector{Float64};
    threshold::Float64=0.001,
    params::HawkesParams=HawkesParams())
    n = length(prices)
    intensities = zeros(Float64, n)
    rh = RollingHawkes(n; params=params, threshold=threshold)

    for i in 1:n
        λ = update!(rh, prices[i], Float64(i))
        intensities[i] = normalized_intensity(λ, params)
    end

    return intensities
end

println("[math] market_hawkes.jl loaded — Hawkes intensity estimator ready")
