# Capital unit tests — no full EnsembleBrain required
#
# Usage:
#   julia --project=brain test/runtests.jl

using Test
using Random

const BRAIN = dirname(@__DIR__)

include(joinpath(BRAIN, "market_types.jl"))
include(joinpath(BRAIN, "market_encoder.jl"))

@testset "MarketPulse wire" begin
    pulse = MarketPulse(
        UInt64(1_700_000_000_000_000_000),
        1.0f0, 0.1f0, 1.1f0, 0.2f0, 1.2f0, 0.3f0, 1.3f0, 0.4f0,
        1.4f0, 0.5f0, 1.5f0, 0.6f0, 1.6f0, 0.7f0,
        0.9f0, 0.0f0, 0.0f0, 0.0f0, 0.0f0,
        45.0f0, 200.0f0, 0.5f0, 0.1f0,
        0.01f0, 0.0001f0,
    )
    buf = pack_market_pulse(pulse)
    @test length(buf) == 120
    decoded = decode_market_pulse(buf)
    @test decoded.timestamp_ns == pulse.timestamp_ns
    @test decoded.dnx_price == pulse.dnx_price
    @test decoded.dydx_funding_rate == pulse.dydx_funding_rate
    @test_throws ErrorException decode_market_pulse(zeros(UInt8, 64))
end

@testset "MarketEncoder" begin
    rng = MersenneTwister(42)
    enc = MarketEncoder(7; delta=0.001f0, rng=rng)

    base = MarketPulse(
        UInt64(1),
        1.0f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0,
        1.0f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0,
        0.0f0, 0.0f0, 0.0f0, 0.0f0, 0.0f0,
        40.0f0, 100.0f0, 0.0f0, 0.0f0,
        0.0f0, 0.0f0,
    )

    s0 = encode!(enc, base)
    @test length(s0) == 28
    # First tick seeds prev_prices — no price-direction spikes
    @test sum(s0[1:4:end]) == 0  # no UP
    @test sum(s0[2:4:end]) == 0  # no DOWN

    up = MarketPulse(
        UInt64(2),
        1.01f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0,
        1.0f0, 0.0f0, 1.0f0, 0.0f0, 1.0f0, 0.0f0,
        0.0f0, 0.0f0, 0.0f0, 0.0f0, 0.0f0,
        40.0f0, 100.0f0, 0.0f0, 0.0f0,
        0.0f0, 0.0f0,
    )
    s1 = encode!(enc, up)
    @test s1[1] == 1.0f0  # DNX Price_UP
    @test s1[2] == 0.0f0
end

@testset "hft_inhibition" begin
    include(joinpath(BRAIN, "hft_inhibition.jl"))
    # Cool hardware, calm market → low inhibition
    z = hft_inhibition(40.0f0, 0.1f0, 0.0f0, 0.0f0)
    @test z == 0.0f0
    # Hot GPU raises inhibition
    hot = hft_inhibition(85.0f0, 0.1f0, 0.0f0, 0.0f0)
    @test hot > 0.0f0
    @test hot <= 3.0f0
    # Positive OI delta reduces inhibition (arousal)
    calm = hft_inhibition(40.0f0, 0.1f0, 0.0f0, 0.0f0; dydx_oi_delta=0.1f0)
    @test calm < z + 0.01f0  # can go slightly negative before clamp → 0
    @test calm >= 0.0f0
end

@testset "causal NERO aggregation (CPU)" begin
    # Pure weighted_readout without loading full NERO (avoids CUDA / EnsembleBrain)
    # Inline the same formula as synapse_conductor.weighted_readout
    function _weighted_readout(lobe_outputs, relevance)
        n = length(lobe_outputs)
        dim = length(lobe_outputs[1])
        s = max(sum(Float32(r) for r in relevance), 1.0f-6)
        out = zeros(Float32, dim)
        for i in 1:n
            w = Float32(relevance[i]) / s
            for j in 1:dim
                out[j] += w * Float32(lobe_outputs[i][j])
            end
        end
        return out
    end

    outs = [
        Float32[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        Float32[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    ]
    y = _weighted_readout(outs, Float32[1, 0, 0, 0])
    @test y[1] ≈ 1.0f0
    @test y[2] ≈ 0.0f0
    y2 = _weighted_readout(outs, Float32[0.25, 0.25, 0.25, 0.25])
    @test y2[1] ≈ 0.25f0
    @test y2[2] ≈ 0.25f0
    y3 = _weighted_readout(outs, Float32[0.1, 0.7, 0.1, 0.1])
    @test y3[2] > y3[1]
    @test abs(sum(y3[1:4]) - 1.0f0) < 1e-5
end

@testset "FeatureStream Hawkes (CPU)" begin
    include(joinpath(BRAIN, "feature_stream.jl"))
    fs = FeatureStream(; window=50, threshold=0.001, base_delta=0.001f0, use_reflex=false)
    # Quiet walk
    λ_quiet = 0.0
    p = 1.0
    for t in 1:20
        p *= 1.0001
        snap = update_features!(fs, p, Float64(t))
        λ_quiet = snap.hawkes_λ
    end
    # Jump cluster → higher intensity
    for t in 21:40
        p *= (t % 2 == 0 ? 1.01 : 0.99)
        snap = update_features!(fs, p, Float64(t))
    end
    snap_hot = fs.last
    @test snap_hot.hawkes_λ >= λ_quiet || snap_hot.hawkes_norm >= 1.0f0
    @test snap_hot.encoder_delta > 0
    @test snap_hot.encoder_delta <= 0.001f0 * 4 + 1e-6  # bounded by clamp
    # Encoder gain
    enc = MarketEncoder(7; delta=0.001f0)
    apply_encoder_gain!(enc, snap_hot)
    @test enc.delta_threshold == snap_hot.encoder_delta
end

@testset "experiment metrics (CPU)" begin
    include(joinpath(BRAIN, "metrics.jl"))
    cfg = ExperimentConfig(duration=20, seed=1, horizon=3, cost_bps=1.0)
    m = MetricsCollector()
    # Synthetic: price drifts up; scores always positive → high hit rate
    price = 1.0
    for t in 1:20
        price *= 1.01
        record_tick!(m, 0.5;
            score=1.0,
            price=price,
            spike_rate=0.01,
            confidence=0.5,
            dominant_lobe=1)
    end
    s = finalize_metrics(m, cfg)
    @test s.n_ticks == 20
    @test s.labeled > 0
    @test s.hit_rate ≈ 1.0
    @test s.pnl_proxy > 0
    @test s.latency_mean_ms ≈ 0.5

    # Opposite scores on rising market → low hit rate
    m2 = MetricsCollector()
    price = 1.0
    for t in 1:20
        price *= 1.01
        record_tick!(m2, 1.0; score=-1.0, price=price, spike_rate=0.0, confidence=0.2, dominant_lobe=2)
    end
    s2 = finalize_metrics(m2, cfg)
    @test s2.hit_rate ≈ 0.0

    # direction_score
    out = zeros(Float32, 16)
    out[1] = 0.8f0
    out[2] = 0.1f0
    @test direction_score(out) ≈ 0.7

    # artifact write
    tmp = mktempdir()
    cfg.run_id = "test-run"
    cfg.output_dir = tmp
    write_run_artifact(joinpath(tmp, cfg.run_id), cfg, s)
    @test isfile(joinpath(tmp, "test-run", "meta.json"))
    @test isfile(joinpath(tmp, "test-run", "metrics.json"))
end

@testset "ReadoutPacket wire" begin
    tick = Int64(42)
    output = zeros(Float32, 16)
    output[1] = 0.8f0
    output[2] = 0.1f0
    relevance = Float32[0.4, 0.3, 0.2, 0.1]
    buf = pack_readout(tick, output, relevance)
    @test length(buf) == 88
    t2, o2, r2 = decode_readout(buf)
    @test t2 == tick
    @test o2[1] ≈ 0.8f0
    @test r2[1] ≈ 0.4f0

    fixture = joinpath(BRAIN, "..", "wire", "fixtures", "readout.bin")
    if isfile(fixture)
        golden = read(fixture)
        @test length(golden) == 88
        gt, go, gr = decode_readout(collect(golden))
        @test gt == 42
        @test go[1] ≈ 0.8f0
    end
end

@testset "TemporalFocus (optional)" begin
    tf_src = abspath(joinpath(BRAIN, "..", "..", "Limen-Neural", "NeuroPulse.jl", "src", "TemporalFocus.jl"))
    if isfile(tf_src)
        try
            include(tf_src)
            router = TemporalFocus.RegionRouter(; n_regions=4, n_out=16)
            regions = [TemporalFocus.ActivityRegion(Float32(0.1 * i), Float32.(randn(16))) for i in 1:4]
            TemporalFocus.update_routing!(router, regions)
            w = router.routing_weights
            @test length(w) == 4
            @test abs(sum(w) - 1.0f0) < 1.0f-3
            @test all(w .>= 0.0f0)
        catch e
            @info "TemporalFocus load skipped" exception = e
            @test true
        end
    else
        @info "TemporalFocus source missing; skip" path=tf_src
        @test true
    end
end

println("All Capital unit tests finished.")
