# market_encoder.jl — Neuromorphic Market-to-Spike Encoding
#
# Converts MarketPulse into a 4×N_asset spike vector for reservoir input.
# Layout per asset: [Price_UP, Price_DOWN, Vol_UP, Vol_DOWN]

using Random

mutable struct MarketEncoder
    n_assets::Int
    prev_prices::Vector{Float32}
    delta_threshold::Float32
    spike_buffer::Vector{Float32}
    rng::AbstractRNG
end

function MarketEncoder(n_assets::Int=N_ASSETS; delta::Float32=0.0005f0, rng::AbstractRNG=Random.default_rng())
    MarketEncoder(
        n_assets,
        zeros(Float32, n_assets),
        delta,
        zeros(Float32, n_assets * 4),
        rng,
    )
end

function encode!(enc::MarketEncoder, pulse::MarketPulse)
    current = copy(pulse.prices)
    vols = copy(pulse.vols)
    length(current) == enc.n_assets && length(vols) == enc.n_assets ||
        error("encoder/universe size mismatch")

    if all(enc.prev_prices .== 0.0f0)
        enc.prev_prices .= current
    end

    enc.spike_buffer .= 0.0f0

    for i in 1:enc.n_assets
        diff = current[i] - enc.prev_prices[i]

        if diff >= enc.delta_threshold
            enc.spike_buffer[4 * (i - 1) + 1] = 1.0f0
            enc.prev_prices[i] = current[i]
        elseif diff <= -enc.delta_threshold
            enc.spike_buffer[4 * (i - 1) + 2] = 1.0f0
            enc.prev_prices[i] = current[i]
        end

        if rand(enc.rng, Float32) < vols[i]
            if diff > 0
                enc.spike_buffer[4 * (i - 1) + 3] = 1.0f0
            else
                enc.spike_buffer[4 * (i - 1) + 4] = 1.0f0
            end
        end
    end

    return enc.spike_buffer
end
