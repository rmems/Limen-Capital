# Capital unit tests — no full EnsembleBrain required

using Test
using Random
using LinearAlgebra

const BRAIN = dirname(@__DIR__)

include(joinpath(BRAIN, "market_types.jl"))
include(joinpath(BRAIN, "market_encoder.jl"))

function sample_pulse(; ts::UInt64 = UInt64(1_700_000_000_000_000_000))
    prices = [1.0f0 + 0.1f0 * i for i in 0:(N_ASSETS - 1)]
    vols = fill(0.1f0, N_ASSETS)
    MarketPulse(; timestamp_ns = ts, prices = prices, vols = vols)
end

@testset "Universe load" begin
    @test N_ASSETS >= 1
    @test N_IN == 4 * N_ASSETS
    @test N_OUT == 2 * (N_ASSETS + 1)
    @test TRADE_UNIVERSE.hash != UInt64(0)
end

@testset "MarketPulse wire v2" begin
    pulse = sample_pulse()
    buf = pack_market_pulse(pulse)
    @test length(buf) == market_pulse_v2_len(N_ASSETS)
    decoded = decode_market_pulse(buf)
    @test decoded.timestamp_ns == pulse.timestamp_ns
    @test decoded.prices == pulse.prices
    @test decoded.dydx_funding_rate == pulse.dydx_funding_rate
    @test_throws ErrorException decode_market_pulse(zeros(UInt8, 64))
end

@testset "MarketPulse validation" begin
    buf = pack_market_pulse(sample_pulse())
    v2 = copy(buf)
    v2[29:32] .= 0x00 # first price word (after header, timestamp, hash, count)
    @test_throws ErrorException decode_market_pulse(v2)

    if N_ASSETS == 7
        golden = joinpath(BRAIN, "..", "wire", "fixtures", "marketpulse.bin")
        raw = collect(read(golden))
        @test length(raw) == 120
        decoded = decode_market_pulse(raw)
        @test decoded.prices[1] ≈ 1.0f0
    end
end

@testset "MarketEncoder" begin
    rng = MersenneTwister(42)
    enc = MarketEncoder(N_ASSETS; delta=0.001f0, rng=rng)
    base = sample_pulse(ts = UInt64(1))
    s0 = encode!(enc, base)
    @test length(s0) == 4 * N_ASSETS
    @test sum(s0[1:4:end]) == 0

    up_prices = copy(base.prices)
    up_prices[1] += 0.02f0
    up = MarketPulse(; timestamp_ns = UInt64(2), prices = up_prices, vols = base.vols)
    s1 = encode!(enc, up)
    @test s1[1] == 1.0f0
end

@testset "hft_inhibition" begin
    include(joinpath(BRAIN, "hft_inhibition.jl"))
    @test hft_inhibition(40.0f0, 0.1f0, 0.0f0, 0.0f0) == 0.0f0
end

@testset "causal NERO aggregation (CPU)" begin
    outs = [
        Float32[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ]
    # The same CUDA-free implementation is included by synapse_conductor.jl.
    include(joinpath(BRAIN, "nero_core.jl"))
    y = weighted_readout(outs, Float32[1, 0, 0, 0])
    @test y == outs[1]
    @test weighted_readout(outs, Float32[4, 3, 2, 1]) ≈
          vcat(Float32[0.4, 0.3, 0.2, 0.1], zeros(Float32, 12))
    @test weighted_readout(outs, zeros(Float32, 4)) == zeros(Float32, 16)
    @test_throws ErrorException weighted_readout(outs, Float32[1])
    @test_throws ErrorException weighted_readout(Vector{Float32}[], Float32[])
end

@testset "FeatureStream Hawkes (CPU)" begin
    include(joinpath(BRAIN, "feature_stream.jl"))
    fs = FeatureStream(; window=50, threshold=0.001, base_delta=0.001f0, use_reflex=false)
    @test fs.last.hawkes_λ == 0.01
    @test hawkes_intensity(Float64[1, 3, 5], 5.0, HawkesParams()) ≈
          0.01 + 0.5 * exp(-0.4) + 0.5 * exp(-0.2)
    @test update_features!(fs, 1.0, 1.0).hawkes_λ ≈ 0.51
    snap = update_features!(fs, 1.01, 2.0)
    @test snap.hawkes_λ ≈ 0.01 + 0.5 * exp(-0.1) + 0.5
    @test snap.hawkes_norm == 8.0f0
    @test snap.encoder_delta == 0.000125f0
    @test snap.reflex_signal == 2.0f0
    # No event at t=3: only the two prior events decay.
    @test update_features!(fs, 1.01, 3.0).hawkes_λ ≈
          0.01 + 0.5 * exp(-0.2) + 0.5 * exp(-0.1)
    # Also exercise gain without saturation, using a nonzero baseline.
    calm = FeatureStream(; params=HawkesParams(baseline_μ=0.5, excitation_α=0.2))
    gain = update_features!(calm, 1.0, 1.0)
    @test gain.hawkes_λ ≈ 0.7
    @test gain.hawkes_norm ≈ 1.4f0
    @test gain.encoder_delta ≈ 0.001f0 / 1.4f0
    @test gain.reflex_signal ≈ 0.4f0
    enc = MarketEncoder(N_ASSETS; delta=0.001f0)
    apply_encoder_gain!(enc, gain)
    @test enc.delta_threshold == gain.encoder_delta
end

@testset "experiment metrics (CPU)" begin
    include(joinpath(BRAIN, "metrics.jl"))
    cfg = ExperimentConfig(duration=20, seed=1, horizon=3, cost_bps=1.0)
    m = MetricsCollector()
    price = 1.0
    for t in 1:20
        price *= 1.01
        record_tick!(m, 0.5; score=1.0, price=price, spike_rate=0.01, confidence=0.5, dominant_lobe=1)
    end
    s = finalize_metrics(m, cfg)
    @test s.n_ticks == 20
    @test s.hit_rate ≈ 1.0
end

@testset "ReadoutPacket wire" begin
    tick = Int64(42)
    output = zeros(Float32, N_OUT)
    output[1] = 0.8f0
    output[2] = 0.1f0
    relevance = Float32[0.4, 0.3, 0.2, 0.1]
    buf = pack_readout(tick, output, relevance)
    @test length(buf) == readout_frame_v2_len(N_PAIRS)
    t2, o2, r2 = decode_readout(buf)
    @test t2 == tick
    @test o2[1] ≈ 0.8f0

    fixture = joinpath(BRAIN, "..", "wire", "fixtures", "readout.bin")
    if N_ASSETS == 7
        gt, go, gr = decode_readout(collect(read(fixture)))
        @test gt == 42
        @test go[1] ≈ 0.8f0
    end
end

@testset "symbol hash stable" begin
    # Independent FNV-1a of the checked-in placeholders joined by newline.
    @test canonical_universe_hash() == 0x72b15f3eee1d443e
    @test TRADE_UNIVERSE.hash == symbol_table_hash(TRADE_UNIVERSE.symbols)
end

include(joinpath(@__DIR__, "wire_hardening.jl"))
include(joinpath(@__DIR__, "wire_fixtures.jl"))
include(joinpath(@__DIR__, "model_math.jl"))
include(joinpath(@__DIR__, "model_math_comparison.jl"))

@testset "Fresh-process backend loading" begin
    fixture_path = joinpath(@__DIR__, "fixtures")
    script = joinpath(@__DIR__, "backend_loading.jl")
    # JULIA_LOAD_PATH is separated by ';' on Windows, ':' elsewhere.
    sep = Sys.iswindows() ? ';' : ':'
    withenv("JULIA_LOAD_PATH" => fixture_path * sep * "@stdlib") do
        @test success(pipeline(`$(Base.julia_cmd()) --startup-file=no --project=@stdlib $script`;
            stdout=stdout, stderr=stderr))
    end
end

println("All Capital unit tests finished.")
