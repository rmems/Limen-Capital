# experiment_runner.jl — Deep ExperimentRunner module
#
# Interface: run_experiment(cfg) -> (summary, artifact_dir)
# Helms are thin CLIs over this module.
#
# Requires: naut_core, reservoir, market_encoder, synapse_conductor, metrics
# Optional spikes: spike_recorder

using CUDA
using Printf
using Statistics
using Random

include(joinpath(@__DIR__, "metrics.jl"))
include(joinpath(@__DIR__, "feature_stream.jl"))

"""
    run_experiment(cfg::ExperimentConfig) -> NamedTuple

Closed-loop simulated market + FeatureStream (Hawkes) + reservoir + causal NERO + metrics.
"""
function run_experiment(cfg::ExperimentConfig)
    Random.seed!(cfg.seed)

    if !isempty(cfg.backend) && cfg.backend != "auto"
        ENV["LIMEN_RESERVOIR"] = cfg.backend
    end

    println()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  ExperimentRunner — SNN HFT research                         ║")
    @printf("║  duration=%-6d seed=%-6d horizon=%-3d backend=%-12s ║\n",
        cfg.duration, cfg.seed, cfg.horizon, cfg.backend)
    println("╚══════════════════════════════════════════════════════════════╝")
    println()

    if !CUDA.functional()
        error("CUDA not available — ExperimentRunner requires a GPU for the reservoir")
    end

    # ── Substrate ──────────────────────────────────────────────────────────
    res = build_reservoir(; n_in=N_IN, n_out=N_OUT)
    ensemble = reservoir_ensemble(res)
    println("[experiment] ", reservoir_status(res))

    encoder = MarketEncoder(N_ASSETS; delta=cfg.delta_encode, rng=MersenneTwister(cfg.seed + 1))
    features = FeatureStream(; base_delta=cfg.delta_encode, use_reflex=false)
    println("[experiment] FeatureStream: Hawkes on  (FastReflex off unless LIMEN_USE_REFLEX=1)")
    nero = NeroOrchestrator()
    metrics = MetricsCollector()
    λ_sum = 0.0

    out_dir = joinpath(cfg.output_dir, cfg.run_id)
    mkpath(out_dir)

    recorder = nothing
    if cfg.record_spikes
        include_once_spike_recorder()
        recorder = SpikeRecorder(joinpath(out_dir, "spikes.bin"))
    end

    current_prices = ones(Float32, N_ASSETS)
    vols = fill(0.005f0, N_ASSETS)
    total_latency = 0.0
    last_tick = 0

    println("[experiment] loop start → $out_dir")
    try
        for tick in 1:cfg.duration
            last_tick = tick
            t0 = time_ns()

            # Market: GBM-ish + scalper feedback
            scalper_rate = Float32(reservoir_lobes(res)[1].last_spike_rate)
            dynamic_vols = vols .* (1.0f0 .+ 5.0f0 * scalper_rate)
            for i in 1:N_ASSETS
                current_prices[i] *= exp(randn(Float32) * dynamic_vols[i])
            end

            # FeatureStream: Hawkes on primary price → encoder + Scalper reflex
            snap = update_features!(features, sum(Float64, current_prices) / N_ASSETS, Float64(tick))
            apply_encoder_gain!(encoder, snap)
            λ_sum += snap.hawkes_λ

            pulse = MarketPulse(;
                timestamp_ns=UInt64(tick),
                prices=current_prices,
                vols=dynamic_vols,
                confidence_signal=0.95f0,
                liquidity_delta=snap.reflex_signal,
                gpu_temp_c=45.0f0,
                gpu_power_w=250.0f0,
                gpu_util_pct=0.8f0,
                basys_buffer_load=0.1f0,
            )

            spikes = encode!(encoder, pulse)
            if features.use_reflex
                update_features!(features, sum(Float64, current_prices) / N_ASSETS, Float64(tick);
                    spikes_cpu=spikes)
            end
            u_gpu = cu(spikes)
            # Hawkes-driven Scalper flash-learning via liquidity_delta / reflex_signal
            reservoir_step!(res, u_gpu, pulse)

            if recorder !== nothing
                for lobe in reservoir_lobes(res)
                    record_spikes!(recorder, Int64(tick), lobe.S)
                end
            end

            output = route_and_aggregate!(nero, ensemble)
            score = direction_score(output)
            conf = Float64(maximum(nero.relevance)) * tanh(abs(score))
            conf = clamp(conf, 0.0, 1.0)
            dom = Int(argmax(nero.relevance))

            dt_ms = (time_ns() - t0) / 1e6
            total_latency += dt_ms
            record_tick!(metrics, dt_ms;
                score=score,
                price=Float64(current_prices[1]),
                spike_rate=Float64(scalper_rate),
                confidence=conf,
                dominant_lobe=dom,
            )

            if cfg.log_every > 0 && tick % cfg.log_every == 0
                @printf("[experiment] t=%4d rate=%.2f%% lat=%.2fms score=%+.3f λ=%.3f δ=%.4f NERO=%s\n",
                    tick, scalper_rate * 100, dt_ms, score, snap.hawkes_λ, snap.encoder_delta,
                    LOBE_NAMES[dom])
            end
        end
    catch e
        if isa(e, InterruptException)
            println("\n[experiment] interrupted at tick $last_tick")
        else
            rethrow(e)
        end
    finally
        if recorder !== nothing
            close_recorder!(recorder)
        end
    end

    summary = finalize_metrics(metrics, cfg)
    extra = Dict{String,Any}(
        "backend_resolved" => string(reservoir_status(res).backend),
        "avg_latency_ms_loop" => last_tick > 0 ? total_latency / last_tick : NaN,
        "hawkes_λ_mean" => last_tick > 0 ? λ_sum / last_tick : NaN,
        "feature_stream" => "hawkes",
        "gpu" => try CUDA.name(CUDA.device()) catch; "unknown" end,
    )
    write_run_artifact(out_dir, cfg, summary; extra=extra)

    println()
    @printf("[experiment] hit_rate=%.3f  labeled=%d  pnl_proxy=%.6f  λ_mean=%.3f\n",
        summary.hit_rate, summary.labeled, summary.pnl_proxy,
        last_tick > 0 ? λ_sum / last_tick : NaN)
    @printf("[experiment] latency mean=%.2fms p50=%.2f p99=%.2f\n",
        summary.latency_mean_ms, summary.latency_p50_ms, summary.latency_p99_ms)
    println("[experiment] artifacts → $out_dir")

    return (summary=summary, dir=out_dir, config=cfg)
end

function include_once_spike_recorder()
    if !@isdefined(SpikeRecorder)
        include(joinpath(@__DIR__, "spike_recorder.jl"))
    end
end
