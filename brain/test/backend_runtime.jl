# Pinned-dependency integration and optional real-GPU acceptance test.
# Run: julia --project=brain brain/test/backend_runtime.jl
using Test
using CUDA

const BRAIN = dirname(@__DIR__)
include(joinpath(BRAIN, "naut_core.jl"))
include(joinpath(BRAIN, "reservoir.jl"))
include(joinpath(BRAIN, "synapse_conductor.jl"))
include(joinpath(BRAIN, "market_encoder.jl"))
include(joinpath(BRAIN, "experiment_runner.jl"))
ENV["LIMEN_VAULT_DIR"] = mktempdir()
include(joinpath(BRAIN, "vault_recorder.jl"))

# The CPU reference backend is covered by the root project, whose pin has ReferenceLSM.
@testset "Pinned LiquidCortex load without name collisions" begin
    @test _LIQUID_CORTEX_AVAILABLE
    @test parentmodule(LiquidCortex.EnsembleBrain) === LiquidCortex
    @test parentmodule(EnsembleBrain) === Main
    @test hasmethod(LiquidCortex.ensemble_step!, Tuple{LiquidCortex.EnsembleBrain, CuVector{Float32}})
end

@testset "Backend-neutral vault and cold covariance (CPU)" begin
    pulse = MarketPulse(; timestamp_ns=UInt64(1), prices=ones(Float32, N_ASSETS),
        vols=fill(0.1f0, N_ASSETS))
    # Neither object is a local EnsembleBrain/SparseBrain.
    lobe = (S=Float32[0, 1, 0, 1], hist_full=false)
    ensemble = (lobes=[lobe],)
    @test compute_reservoir_covariance!(lobe) === nothing
    vr = VaultRecorder(101)
    try
        for _ in 1:100
            record_tick!(vr, pulse, ensemble)
        end
        @test vr.tick_count == 100
        @test length(vr.quote_buffer) == 100 * N_ASSETS
        @test length(vr.meta_buffer) == 100
        @test length(vr.spike_buffer) == 200
        @test vr.spike_buffer[1:2] == [(1, Int32(1), Int8(1)), (1, Int32(3), Int8(1))]
        flush_vault!(vr)
        @test first(DuckDB.DBInterface.execute(vr.con,
            "SELECT COUNT(*) FROM market_tick_meta"))[1] == 100
    finally
        close_vault!(vr)
    end
end

@testset "Real auto LiquidCortex, recording and reflex: 100 ticks (GPU)" begin
    if CUDA.functional()
        withenv("LIMEN_RESERVOIR" => "auto") do
            res = build_reservoir(; n_in=N_IN, n_out=N_OUT)
            @test res.backend === :liquid_cortex
            nero = NeroOrchestrator()
            fs = FeatureStream(; use_reflex=true)
            @test fs.use_reflex
            recorder = SpikeRecorder(joinpath(mktempdir(), "spikes.bin"))
            vr = VaultRecorder(100)
            start_tick = vr.tick_count
            try
                for tick in 1:100
                    pulse = MarketPulse(; timestamp_ns=UInt64(tick),
                        prices=ones(Float32, N_ASSETS), vols=fill(0.1f0, N_ASSETS))
                    spikes = ones(Float32, N_IN)
                    reservoir_step!(res, cu(spikes), pulse)
                    @test all(isfinite, reservoir_output(res))
                    @test route_and_aggregate!(nero, reservoir_ensemble(res)) !== nothing
                    record_tick!(vr, pulse, reservoir_ensemble(res))
                    for lobe in reservoir_lobes(res)
                        record_spikes!(recorder, Int64(tick), lobe.S)
                        tick % 50 == 0 && @test compute_reservoir_covariance!(lobe) === nothing
                    end
                    snap = update_features!(fs, 1.0, Float64(tick); spikes_cpu=spikes)
                    @test all(isfinite, snap.reflex_features)
                end
                @test vr.tick_count == start_tick + 100
            finally
                close_recorder!(recorder)
                close_vault!(vr)
            end
        end
    else
        @info "GPU acceptance not run: no functional CUDA device"
        @test_skip CUDA.functional()
    end
end
