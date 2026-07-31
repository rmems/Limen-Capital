# feature_stream.jl — Math features on the live research path
#
# Currently wired:
#   RollingHawkes (math/market_hawkes.jl) — self-excitation intensity λ(t)
#     → encoder delta gain (more sensitive when λ high)
#     → Scalper reflex_signal (flash-learning when λ high)
#
# Optional (GPU):
#   FastReflex (math/market_lsm.jl) — 256-neuron preprocessor on spike vector
#     enable with FeatureStream(...; use_reflex=true) or LIMEN_USE_REFLEX=1
#
# Not wired (available offline only):
#   market_fractal.jl, market_sde.jl — demote until a second consumer exists

const _MATH_DIR = joinpath(@__DIR__, "..", "math")

if !@isdefined(RollingHawkes)
    include(joinpath(_MATH_DIR, "market_hawkes.jl"))
end

"""
    FeatureSnapshot

Per-tick outputs from the feature stream.
"""
struct FeatureSnapshot
    hawkes_λ::Float64
    hawkes_norm::Float32          # λ / baseline, ≥ 1 when excited
    encoder_delta::Float32        # suggested MarketEncoder delta threshold
    reflex_signal::Float32        # feed reservoir_step! liquidity/reflex path
    reflex_features::Union{Nothing,Vector{Float32}}  # 16-d if FastReflex on
end

"""
    FeatureStream

Streaming feature extractor sitting between MarketEncoder and Reservoir.
"""
mutable struct FeatureStream
    hawkes::RollingHawkes
    base_delta::Float32
    λ_baseline::Float64
    use_reflex::Bool
    reflex::Any                   # FastReflex or nothing
    last::FeatureSnapshot
end

"""
    FeatureStream(; window=100, threshold=0.001, base_delta=0.001f0, use_reflex=false)
"""
function FeatureStream(;
    window::Int=100,
    threshold::Float64=0.001,
    base_delta::Float32=0.001f0,
    use_reflex::Bool=false,
    params::HawkesParams=HawkesParams(),
)
    use_r = use_reflex || get(ENV, "LIMEN_USE_REFLEX", "0") == "1"
    reflex = nothing
    if use_r
        try
            if !@isdefined(FastReflex)
                include(joinpath(_MATH_DIR, "market_lsm.jl"))
            end
            reflex = FastReflex()
        catch e
            @warn "FastReflex unavailable; continuing Hawkes-only" exception = e
            use_r = false
        end
    end
    snap0 = FeatureSnapshot(params.baseline_μ, 1.0f0, base_delta, 0.0f0, nothing)
    FeatureStream(
        RollingHawkes(window; params=params, threshold=threshold),
        base_delta,
        max(params.baseline_μ, 1e-9),
        use_r,
        reflex,
        snap0,
    )
end

"""
    update_features!(fs, price, timestamp; spikes_cpu=nothing) -> FeatureSnapshot

`price` — primary asset (e.g. DNX). `timestamp` — tick index as Float64.
If `use_reflex` and `spikes_cpu` is a 28-vector, run FastReflex.
"""
function update_features!(
    fs::FeatureStream,
    price::Float64,
    timestamp::Float64;
    spikes_cpu::Union{Nothing,AbstractVector{Float32}}=nothing,
)
    λ = update!(fs.hawkes, price, timestamp)
    λn = Float32(clamp(λ / fs.λ_baseline, 0.25, 8.0))
    # Excited market → smaller delta → more price spikes into the reservoir
    delta = fs.base_delta / λn
    # Excess intensity drives Scalper flash-learning (same gate as |liquidity| > 0.1)
    reflex_sig = Float32(clamp((λ / fs.λ_baseline) - 1.0, 0.0, 2.0))

    feats = nothing
    if fs.use_reflex && fs.reflex !== nothing && spikes_cpu !== nothing
        if length(spikes_cpu) == 28
            feats = step_cpu(fs.reflex, Vector{Float32}(spikes_cpu))
        end
    end

    fs.last = FeatureSnapshot(λ, λn, delta, reflex_sig, feats)
    return fs.last
end

"""
    apply_encoder_gain!(encoder, snap)

Set MarketEncoder delta from Hawkes-adapted threshold.
"""
function apply_encoder_gain!(encoder, snap::FeatureSnapshot)
    encoder.delta_threshold = snap.encoder_delta
    return nothing
end
