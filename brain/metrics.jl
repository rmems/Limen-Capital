# metrics.jl — Experiment metrics (pure CPU, no CUDA)
#
# Walk-forward style labels from future returns; direction from readout scores.
# Used by experiment_runner.jl; unit-testable offline.

using Statistics
using Dates
using Random
using Printf

"""
    ExperimentConfig

Versioned knobs for a research run. All fields serializable to meta.json.
"""
Base.@kwdef mutable struct ExperimentConfig
    duration::Int = 200
    seed::Int = 42
    horizon::Int = 5              # ticks ahead for direction label
    cost_bps::Float64 = 1.0       # round-trip friction for PnL proxy
    delta_encode::Float32 = 0.001f0
    n_in::Int = 28
    n_out::Int = 16
    record_spikes::Bool = false
    log_every::Int = 50
    output_dir::String = "runs"
    run_id::String = ""
    backend::String = "auto"      # auto | liquid_cortex | naut_core
end

"""
    MetricsCollector

Online accumulators. Finalize after the run with `finalize_metrics`.
"""
mutable struct MetricsCollector
    n_ticks::Int
    latencies_ms::Vector{Float64}
    spike_rates::Vector{Float64}       # scalper (or primary lobe) rate
    confidences::Vector{Float64}
    # deferred labels: store pred score and price at t; score at end with horizon
    scores::Vector{Float64}
    prices::Vector{Float64}            # primary asset mid
    relevance_dom::Vector{Int}
end

MetricsCollector() = MetricsCollector(0, Float64[], Float64[], Float64[], Float64[], Float64[], Int[])

"""
    direction_score(output) -> Float64

Sum of bull−bear over 8 pairs (same convention as NervousWire).
"""
function direction_score(output::AbstractVector{<:Real})::Float64
    n = length(output)
    s = 0.0
    i = 1
    while i + 1 <= n
        s += Float64(output[i]) - Float64(output[i + 1])
        i += 2
    end
    return s
end

"""
    record_tick!(m, latency_ms; score, price, spike_rate, confidence, dominant_lobe)
"""
function record_tick!(
    m::MetricsCollector,
    latency_ms::Float64;
    score::Float64,
    price::Float64,
    spike_rate::Float64=0.0,
    confidence::Float64=0.0,
    dominant_lobe::Int=1,
)
    m.n_ticks += 1
    push!(m.latencies_ms, latency_ms)
    push!(m.scores, score)
    push!(m.prices, price)
    push!(m.spike_rates, spike_rate)
    push!(m.confidences, confidence)
    push!(m.relevance_dom, dominant_lobe)
    return nothing
end

"""
    finalize_metrics(m, cfg) -> NamedTuple

Walk-forward hit-rate and PnL proxy using `cfg.horizon` future returns.
"""
function finalize_metrics(m::MetricsCollector, cfg::ExperimentConfig)
    h = cfg.horizon
    n = m.n_ticks
    hits = 0
    labeled = 0
    pnl = 0.0
    cost = cfg.cost_bps * 1e-4

    for t in 1:(n - h)
        p0 = m.prices[t]
        p1 = m.prices[t + h]
        p0 > 0 || continue
        ret = (p1 - p0) / p0
        label = ret > 1e-12 ? 1 : (ret < -1e-12 ? -1 : 0)
        sc = m.scores[t]
        pred = sc > 1e-6 ? 1 : (sc < -1e-6 ? -1 : 0)
        if pred != 0 && label != 0
            labeled += 1
            if pred == label
                hits += 1
            end
            pnl += pred * ret - abs(pred) * cost
        elseif pred != 0
            # no clear label — still pay cost if we "traded"
            pnl += -abs(pred) * cost
        end
    end

    hit_rate = labeled > 0 ? hits / labeled : NaN
    lat = m.latencies_ms
    sort!(copy(lat))
    function pct(xs, p)
        isempty(xs) && return NaN
        idx = clamp(ceil(Int, p / 100 * length(xs)), 1, length(xs))
        ys = sort(xs)
        return ys[idx]
    end

    return (
        n_ticks = n,
        labeled = labeled,
        hits = hits,
        hit_rate = hit_rate,
        pnl_proxy = pnl,
        latency_mean_ms = isempty(lat) ? NaN : mean(lat),
        latency_p50_ms = pct(lat, 50),
        latency_p99_ms = pct(lat, 99),
        spike_rate_mean = isempty(m.spike_rates) ? NaN : mean(m.spike_rates),
        confidence_mean = isempty(m.confidences) ? NaN : mean(m.confidences),
        horizon = h,
        cost_bps = cfg.cost_bps,
        seed = cfg.seed,
    )
end

"""
    write_run_artifact(dir, cfg, summary; extra=Dict())

Write `meta.json` + `metrics.json` under `dir`.
"""
function write_run_artifact(dir::String, cfg::ExperimentConfig, summary; extra=Dict{String,Any}())
    mkpath(dir)
    meta = Dict{String,Any}(
        "run_id" => cfg.run_id,
        "seed" => cfg.seed,
        "duration" => cfg.duration,
        "horizon" => cfg.horizon,
        "cost_bps" => cfg.cost_bps,
        "backend" => cfg.backend,
        "n_in" => cfg.n_in,
        "n_out" => cfg.n_out,
        "record_spikes" => cfg.record_spikes,
        "timestamp_utc" => string(Dates.now(Dates.UTC)),
        "git_sha" => try
            strip(read(`git -C $(dirname(@__DIR__)) rev-parse --short HEAD`, String))
        catch
            "unknown"
        end,
    )
    merge!(meta, extra)
    open(joinpath(dir, "meta.json"), "w") do io
        _write_json(io, meta)
    end
    # summary as Dict
    sumd = Dict{String,Any}(string(k) => v for (k, v) in pairs(summary))
    open(joinpath(dir, "metrics.json"), "w") do io
        _write_json(io, sumd)
    end
    return dir
end

# Minimal JSON writer (no JSON.jl dependency)
function _write_json(io::IO, x)
    if x isa AbstractDict
        print(io, "{")
        first = true
        for (k, v) in x
            first || print(io, ",")
            first = false
            print(io, "\"", k, "\":")
            _write_json(io, v)
        end
        print(io, "}")
    elseif x isa AbstractVector
        print(io, "[")
        for (i, v) in enumerate(x)
            i > 1 && print(io, ",")
            _write_json(io, v)
        end
        print(io, "]")
    elseif x isa AbstractString
        print(io, "\"", replace(x, "\"" => "\\\""), "\"")
    elseif x isa Bool
        print(io, x ? "true" : "false")
    elseif x isa Integer
        print(io, x)
    elseif x isa AbstractFloat
        if isnan(x)
            print(io, "null")
        else
            @printf(io, "%.8g", x)
        end
    elseif x === nothing
        print(io, "null")
    else
        print(io, "\"", x, "\"")
    end
end

function parse_research_args(args::Vector{String})
    cfg = ExperimentConfig()
    i = 1
    while i <= length(args)
        a = args[i]
        if a in ("--duration", "-n") && i < length(args)
            cfg.duration = parse(Int, args[i + 1]); i += 2
        elseif a == "--seed" && i < length(args)
            cfg.seed = parse(Int, args[i + 1]); i += 2
        elseif a == "--horizon" && i < length(args)
            cfg.horizon = parse(Int, args[i + 1]); i += 2
        elseif a == "--cost-bps" && i < length(args)
            cfg.cost_bps = parse(Float64, args[i + 1]); i += 2
        elseif a == "--output-dir" && i < length(args)
            cfg.output_dir = args[i + 1]; i += 2
        elseif a == "--record-spikes"
            cfg.record_spikes = true; i += 1
        elseif a == "--backend" && i < length(args)
            cfg.backend = args[i + 1]; i += 2
        elseif a == "--log-every" && i < length(args)
            cfg.log_every = parse(Int, args[i + 1]); i += 2
        elseif a in ("--help", "-h")
            println("""
research_helm / experiment_runner options:
  --duration N       ticks (default 200)
  --seed S           RNG seed (default 42)
  --horizon H        label horizon ticks (default 5)
  --cost-bps X       friction bps (default 1.0)
  --output-dir DIR   artifact directory (default runs/)
  --record-spikes    write binary spike log
  --backend NAME     auto|liquid_cortex|naut_core
  --log-every N      print every N ticks
""")
            exit(0)
        else
            i += 1
        end
    end
    if isempty(cfg.run_id)
        cfg.run_id = string(Dates.format(Dates.now(), "yyyymmdd-HHMMSS"), "-s", cfg.seed)
    end
    return cfg
end
