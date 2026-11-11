# Run in a fresh process with only the fixture package directory + stdlib.
using Test

module LoadingHarness
const BRAIN = dirname(@__DIR__)
# A local type must not be replaced by LiquidCortex's exported EnsembleBrain.
mutable struct EnsembleBrain
    ticks::Int
end
EnsembleBrain() = EnsembleBrain(0)
ensemble_step!(ens::EnsembleBrain, u; kwargs...) = (ens.ticks += 1; nothing)
get_ensemble_output(ens::EnsembleBrain) = zeros(Float32, 16)
ensemble_diagnostics(ens::EnsembleBrain) = (ticks=ens.ticks,)
include(isempty(ARGS) ? joinpath(BRAIN, "reservoir.jl") : only(ARGS))

function first_call()
    res = build_reservoir()
    @assert res.backend === :liquid_cortex
    @assert res.ensemble isa LiquidCortex.EnsembleBrain
    for _ in 1:100
        reservoir_step!(res, ones(Float32, 28); inhibition=0.0f0)
        @assert reservoir_output(res) == fill(28.0f0, 16)
    end
    return reservoir_diagnostics(res)
end
end

@testset "auto backend first call, module isolation, 100 ticks (API double)" begin
    @test !isdefined(Main, :LiquidCortex)
    withenv("LIMEN_RESERVOIR" => "auto") do
        @test LoadingHarness.first_call().ticks == 100
    end
    @test parentmodule(LoadingHarness.EnsembleBrain) === LoadingHarness
    withenv("LIMEN_RESERVOIR" => "naut_core") do
        res = LoadingHarness.build_reservoir()
        @test res.backend === :naut_core
        @test res.ensemble isa LoadingHarness.EnsembleBrain
        LoadingHarness.reservoir_step!(res, ones(Float32, 28))
        @test LoadingHarness.reservoir_diagnostics(res).ticks == 1
        @test LoadingHarness.reservoir_output(res) == zeros(Float32, 16)
    end
end

# Exercise the actual lazy-include boundary without requiring a CUDA device.
const REFLEX_ROOT = mktempdir()
mkpath(joinpath(REFLEX_ROOT, "brain"))
mkpath(joinpath(REFLEX_ROOT, "math"))
cp(joinpath(dirname(@__DIR__), "feature_stream.jl"),
    joinpath(REFLEX_ROOT, "brain", "feature_stream.jl"))
cp(joinpath(dirname(@__DIR__), "..", "math", "market_hawkes.jl"),
    joinpath(REFLEX_ROOT, "math", "market_hawkes.jl"))
write(joinpath(REFLEX_ROOT, "math", "market_lsm.jl"), """
    struct FastReflex end
    step_cpu(::FastReflex, u) = fill(sum(u), 16)
    """)

module ReflexHarness
include(joinpath(Main.REFLEX_ROOT, "brain", "feature_stream.jl"))
function first_call()
    fs = FeatureStream(; use_reflex=true)
    @assert fs.use_reflex
    @assert FeatureStream(; use_reflex=true).use_reflex
    return update_features!(fs, 1.0, 1.0; spikes_cpu=ones(Float32, 28))
end
end

@testset "FastReflex lazy binding and methods visible on first call" begin
    @test ReflexHarness.first_call().reflex_features == fill(28.0f0, 16)
end
