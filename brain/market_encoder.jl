# market_encoder.jl — Neuromorphic Market-to-Spike Encoding
#
# Converts MarketPulse into a 28-element spike vector for reservoir input.
# Layout per asset: [Price_UP, Price_DOWN, Vol_UP, Vol_DOWN]
#
# Requires MarketPulse (from market_types.jl or naut_core.jl).

using Random

mutable struct MarketEncoder
    n_assets::Int
    prev_prices::Vector{Float32}
    delta_threshold::Float32
    spike_buffer::Vector{Float32}  # 28-element binary spike vector
    rng::AbstractRNG
end

"""
    MarketEncoder(n_assets=7; delta=0.0005f0, rng=Random.default_rng())

Initialize the encoder for DNX, Quai, Qubic, Kaspa, Monero, Ocean, Verus.
Pass a seeded `rng` for reproducible vol-channel Poisson spikes in research.
"""
function MarketEncoder(n_assets::Int=7; delta::Float32=0.0005f0, rng::AbstractRNG=Random.default_rng())
    MarketEncoder(
        n_assets,
        zeros(Float32, n_assets),
        delta,
        zeros(Float32, n_assets * 4),
        rng,
    )
end

"""
    encode!(encoder, pulse::MarketPulse) -> Vector{Float32}

Processes a MarketPulse and returns a 28-element spike vector.
Layout per asset: [Price_UP, Price_DOWN, Vol_UP, Vol_DOWN]
"""
function encode!(enc::MarketEncoder, pulse::MarketPulse)
    current = Float32[
        pulse.dnx_price, pulse.quai_price, pulse.qubic_price,
        pulse.kaspa_price, pulse.monero_price, pulse.ocean_price, pulse.verus_price
    ]
    vols = Float32[
        pulse.dnx_vol, pulse.quai_vol, pulse.qubic_vol,
        pulse.kaspa_vol, pulse.monero_vol, pulse.ocean_vol, pulse.verus_vol
    ]

    # Initialize prev_prices on first tick
    if all(enc.prev_prices .== 0.0f0)
        enc.prev_prices .= current
    end

    enc.spike_buffer .= 0.0f0

    for i in 1:enc.n_assets
        diff = current[i] - enc.prev_prices[i]

        if diff >= enc.delta_threshold
            enc.spike_buffer[4*(i-1) + 1] = 1.0f0 # Price_UP
            enc.prev_prices[i] = current[i]
        elseif diff <= -enc.delta_threshold
            enc.spike_buffer[4*(i-1) + 2] = 1.0f0 # Price_DOWN
            enc.prev_prices[i] = current[i]
        end

        # Poisson/rate encoding on volatility channels
        if rand(enc.rng, Float32) < vols[i]
            if diff > 0
                enc.spike_buffer[4*(i-1) + 3] = 1.0f0 # Volatility_Rising
            else
                enc.spike_buffer[4*(i-1) + 4] = 1.0f0 # Volatility_Falling
            end
        end
    end

    return enc.spike_buffer
end
