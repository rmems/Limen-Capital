const FEATURE_ORDER = ("spread_ticks", "mid_ticks", "microprice_ticks", "imbalance", "signed_order_flow")
const CLASS_ORDER = (Down, Flat, Up)

"""Market glue over LiquidCortex state and DendriteTrader's trained ridge readout.

Five causal inputs, five reservoir projections, three direction classes. This is
a tanh reference reservoir, not a claim that the CPU backend implements LIF spikes.
"""
mutable struct ReservoirForecast <: AbstractForecastModel
    seed::Int
    encoding::Symbol
    temporal::Bool
    reservoir::Reservoir
    encoder::DeltaEncoder
    readout::RidgeClassifier
    scales::Vector{Float64}
    horizon::Union{Nothing,ForecastHorizon}
    last_sequence::Int64
    last_timestamp::Int64
end

function ReservoirForecast(; seed=11, encoding=:delta, temporal=true)
    encoding in (:delta, :raw) || throw(ArgumentError("encoding must be delta or raw"))
    reservoir = build_reservoir(; backend=:reference_cpu, n_in=5, n_out=5,
        neurons=32, rng=MersenneTwister(seed))
    ReservoirForecast(seed, encoding, temporal, reservoir,
        DeltaEncoder(; threshold=0.05, saturation=1), RidgeClassifier(1.0),
        ones(5), nothing, 0, 0)
end

feature_values(row::FeatureRow) = Float64[getproperty(row, Symbol(k)) for k in FEATURE_ORDER]
asrow(row::FeatureRow, x) = FeatureRow(row.exchange_ts_ns, row.sequence, x...)

function reset_state!(model::ReservoirForecast)
    reset_reservoir!(model.reservoir)
    reset_state!(model.encoder)
    model.last_sequence = 0
    model.last_timestamp = 0
    return model
end

function project!(model::ReservoirForecast, frame::FeatureFrame)
    rows = FeatureRow[]
    for row in frame
        row.sequence > model.last_sequence || throw(ArgumentError("reset before replaying a range"))
        row.exchange_ts_ns >= model.last_timestamp || throw(ArgumentError("timestamp decreased"))
        scaled = feature_values(row) ./ model.scales
        u = if model.encoding === :delta
            vec(encode!(model.encoder, FeatureFrame([asrow(row, scaled)])).spikes)
        else
            scaled
        end
        model.temporal || reset_reservoir!(model.reservoir)
        # No pulse/hardware telemetry or inhibition enters this experiment.
        reservoir_step!(model.reservoir, Float32.(u); inhibition=0.0f0)
        push!(rows, asrow(row, reservoir_output(model.reservoir)))
        model.last_sequence = row.sequence
        model.last_timestamp = row.exchange_ts_ns
    end
    return FeatureFrame(rows)
end

function fit!(model::ReservoirForecast, frame::FeatureFrame, targets::AbstractVector{MovementTarget})
    isempty(frame) && throw(ArgumentError("empty training frame"))
    sequences = Set(row.sequence for row in frame)
    isempty(targets) && throw(ArgumentError("empty training targets"))
    all(t -> t.anchor_sequence in sequences && t.target_sequence in sequences &&
        t.target_sequence > t.anchor_sequence, targets) ||
        throw(ArgumentError("targets must remain wholly inside training"))
    horizons = unique(t.horizon_events for t in targets)
    length(horizons) == 1 || throw(ArgumentError("mixed horizons"))
    model.scales = [max(maximum(abs(feature_values(row)[j]) for row in frame), 1e-12) for j in 1:5]
    reset_state!(model)
    fit!(model.readout, project!(model, frame), targets)
    model.horizon = ForecastHorizon(only(horizons))
    reset_state!(model)
    return model
end

function predict!(model::ReservoirForecast, frame::FeatureFrame, horizon::ForecastHorizon)
    model.horizon === nothing && throw(ArgumentError("fit before prediction"))
    horizon == model.horizon || throw(ArgumentError("horizon mismatch"))
    predict!(model.readout, project!(model, frame), horizon)
end

"""Identity of fitted weights/configuration, deliberately excluding transient state."""
function checkpoint_id(model::ReservoirForecast)
    bytes2hex(sha256(JSON3.write((seed=model.seed, encoding=model.encoding,
        temporal=model.temporal, horizon=model.horizon, scales=model.scales,
        W=model.reservoir.ensemble.W, Win=model.reservoir.ensemble.Win,
        Wout=model.reservoir.ensemble.Wout, weights=model.readout.weights,
        readout_scales=model.readout.scales))))
end

function qualify_backend()
    model = ReservoirForecast()
    r = model.reservoir
    reservoir_step!(r, ones(Float32, 5); inhibition=0.0f0)
    first_output = reservoir_output(r)
    reset_reservoir!(r)
    reservoir_step!(r, ones(Float32, 5); inhibition=0.0f0)
    first_output == reservoir_output(r) || error("non-deterministic reset")
    length(first_output) == 5 && all(isfinite, first_output) || error("invalid output")
    any(!iszero, first_output) || error("inactive reservoir")
    return (backend="LiquidCortex.ReferenceLSM", device="CPU", neurons=32,
        n_in=5, n_out=5, reset_repeatable=true, finite_active_output=true)
end
