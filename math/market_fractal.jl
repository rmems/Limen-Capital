# market_fractal.jl — Hurst Exponent & Fractal Dimension Analysis
#
# Computes the Hurst exponent H from a price series using rescaled range
# (R/S) analysis. H characterizes the long-range dependence of a time series:
#
#   H > 0.5  → Trending (persistent) — price moves tend to continue
#   H ≈ 0.5  → Random walk (Brownian) — no memory
#   H < 0.5  → Mean-reverting (anti-persistent) — price moves tend to reverse
#
# Fractal dimension D = 2 - H (for a time series embedded in 2D)
#
# Integration with Spikenaut Brain:
#   - Regime detection: trending (H>0.55) vs mean-reverting (H<0.45)
#   - Adaptive time constants: high H → favor Macro lobe, low H → favor Scalper
#   - Kelly sizing adjustment: reduce bet size when H ≈ 0.5 (uncertain regime)

using Statistics

# ─── Hurst Exponent (R/S Method) ─────────────────────────────────────────────

"""
    hurst_rs(data; min_window=20) -> Float64

Compute the Hurst exponent using rescaled range (R/S) analysis.

The method splits the series into windows of size n, computes R/S for each,
then fits H via: log(R/S) = H · log(n) + c.

# Arguments
- `data::AbstractVector{Float64}`: price or return series
- `min_window::Int`: minimum window size (default 20)

# Returns
- H ∈ [0.0, 1.0]: Hurst exponent

# Example
```julia
prices = cumsum(randn(1000))  # random walk → H ≈ 0.5
H = hurst_rs(prices)
```
"""
function hurst_rs(data::AbstractVector{Float64}; min_window::Int=20)
    n = length(data)
    if n < 2 * min_window
        return 0.5  # insufficient data
    end

    # Compute log(R/S) for various window sizes
    log_n = Float64[]
    log_rs = Float64[]

    # Use powers of 2 for window sizes (standard practice)
    max_power = floor(Int, log2(n))
    for p in 2:max_power
        win_size = 2^p
        if win_size > n ÷ 2
            break
        end

        # Compute R/S for each non-overlapping window
        n_windows = n ÷ win_size
        rs_values = Float64[]

        for w in 1:n_windows
            start_idx = (w - 1) * win_size + 1
            end_idx = w * win_size
            segment = @view data[start_idx:end_idx]

            # Mean of the segment
            μ = mean(segment)

            # Cumulative deviation from mean
            cum_dev = cumsum(segment .- μ)

            # Range: max - min of cumulative deviation
            R = maximum(cum_dev) - minimum(cum_dev)

            # Standard deviation
            S = std(segment; mean=μ, corrected=false)

            if S > 1e-10
                push!(rs_values, R / S)
            end
        end

        if !isempty(rs_values)
            push!(log_n, log(Float64(win_size)))
            push!(log_rs, log(mean(rs_values)))
        end
    end

    if length(log_n) < 2
        return 0.5
    end

    # Linear regression: log(R/S) = H · log(n) + c
    # H = slope of the line
    x = log_n
    y = log_rs
    n_pts = length(x)

    x_mean = mean(x)
    y_mean = mean(y)

    ss_xy = sum((x[i] - x_mean) * (y[i] - y_mean) for i in 1:n_pts)
    ss_xx = sum((x[i] - x_mean)^2 for i in 1:n_pts)

    H = ss_xx > 1e-10 ? ss_xy / ss_xx : 0.5

    return clamp(H, 0.0, 1.0)
end

# ─── Rolling Hurst for Streaming Data ─────────────────────────────────────────

"""
    RollingHurst(window_size; min_window)

Streaming Hurst exponent estimator using a rolling window.
Call `update!` each tick to get the current regime estimate.

Fields:
  prices      — rolling price buffer (circular)
  window_size — total buffer size
  min_window  — minimum segment size for R/S
  pos         — current write position
  count       — total ticks seen
"""
mutable struct RollingHurst
    prices::Vector{Float64}
    window_size::Int
    min_window::Int
    pos::Int
    count::Int
end

"""
    RollingHurst(window_size=512; min_window=32) -> RollingHurst

Create a streaming Hurst estimator with a rolling window.
"""
function RollingHurst(window_size::Int=512; min_window::Int=32)
    RollingHurst(zeros(Float64, window_size), window_size, min_window, 1, 0)
end

"""
    update!(rh, price) -> Float64

Process a new price tick. Returns the current Hurst exponent H.
"""
function update!(rh::RollingHurst, price::Float64)
    rh.prices[rh.pos] = price
    rh.count += 1
    rh.pos = rh.pos >= rh.window_size ? 1 : rh.pos + 1

    # Need enough data for R/S analysis
    if rh.count < 2 * rh.min_window
        return 0.5
    end

    # Extract the rolling window
    if rh.count >= rh.window_size
        window = rh.prices
    else
        window = @view rh.prices[1:rh.count]
    end

    return hurst_rs(window; min_window=rh.min_window)
end

# ─── Regime Classification ────────────────────────────────────────────────────

"""
    regime_from_hurst(H) -> Symbol

Classify the current market regime based on the Hurst exponent.

Returns:
  :trending       — H > 0.55 (persistent, momentum-favorable)
  :random         — 0.45 ≤ H ≤ 0.55 (random walk, uncertain)
  :mean_reverting — H < 0.45 (anti-persistent, contrarian-favorable)
"""
function regime_from_hurst(H::Float64)
    if H > 0.55
        return :trending
    elseif H < 0.45
        return :mean_reverting
    else
        return :random
    end
end

"""
    regime_weight(H) -> Float64

Returns a regime confidence weight in [0, 1].
Higher when H is far from 0.5 (more certain regime).
Peak confidence at H=0.0 or H=1.0.
"""
function regime_weight(H::Float64)
    return 2.0 * abs(H - 0.5)  # 0.0 at H=0.5, 1.0 at H=0.0 or H=1.0
end

# ─── Batch Computation (for backtesting) ─────────────────────────────────────

"""
    compute_hurst_series(prices; window_size, min_window) -> Vector{Float64}

Compute a rolling Hurst exponent series from a price array.
Returns a vector of H values of the same length as prices.
"""
function compute_hurst_series(prices::AbstractVector{Float64};
    window_size::Int=512,
    min_window::Int=32)
    n = length(prices)
    H_series = fill(0.5, n)
    rh = RollingHurst(window_size; min_window=min_window)

    for i in 1:n
        H_series[i] = update!(rh, prices[i])
    end

    return H_series
end

println("[math] market_fractal.jl loaded — Hurst exponent estimator ready")
