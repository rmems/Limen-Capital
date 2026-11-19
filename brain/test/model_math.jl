module ModelMathTests
using Test, LinearAlgebra, SparseArrays, Random, Printf
include(joinpath(@__DIR__, "..", "model_math.jl"))
const N_LOBES = 4
const N_OUT = 16
include(joinpath(@__DIR__, "..", "synapse_conductor.jl"))

@testset "Spectral scaling uses neuron count, not nonzero count" begin
    # Hadamard blocks have known eigenvalue magnitudes sqrt(4) = 2.
    H = Float32[1 1 1 1; 1 -1 1 -1; 1 1 -1 -1; 1 -1 -1 1]
    for blocks in (1, 8)
        W = kron(sparse(I, blocks, blocks), sparse(H))
        rho = maximum(abs, eigvals(Matrix(W)))
        @test spectral_radius_proxy(W) ≈ rho ≈ 2
        old_proxy = norm(W) / sqrt(nnz(W))
        @test rho / old_proxy ≈ sqrt(nnz(W) / size(W, 1)) rtol=1f-6
        W .*= 0.9f0 / spectral_radius_proxy(W)
        @test maximum(abs, eigvals(Matrix(W))) ≈ 0.9f0
        @test spectral_radius_proxy(W) ≈ 0.9f0
    end
    @test spectral_radius_proxy(spzeros(Float32, 8, 8)) == 0
    @test isfinite(spectral_radius_proxy(fill(Float16(256), 4, 4)))
    @test spectral_radius_proxy(fill(Float16(256), 4, 4)) == 512
    @test_throws ArgumentError spectral_radius_proxy(zeros(2, 3))
    @test_throws ArgumentError spectral_radius_proxy(zeros(0, 0))
end

@testset "Balanced readout learning, traces and current readout" begin
    W = Float32[0.2 -0.2; -0.2 0.2]
    pre, post, output = zeros(Float32, 2), zeros(Float32, 2), fill(99.0f0, 2)
    original = copy(W)
    # A first isolated event establishes both pre and output-specific post traces.
    update_readout!(W, pre, post, Float32[1, 0], output;
        decay=0.9f0, eta=0.1f0, w_max=1.0f0, learn=false)
    @test W == original
    @test pre == Float32[1, 0]
    @test post == Float32[1, 0]
    @test output == Float32[0.2, -0.2] # not stale caller-supplied output
    update_readout!(W, pre, post, Float32[0, 1], output;
        decay=0.9f0, eta=0.1f0, w_max=1.0f0, learn=true)
    @test W[1, 2] ≈ -0.29f0 # anti-causal pair depresses
    @test W[2, 1] ≈ -0.11f0 # causal pair potentiates
    @test W[1, 1] == original[1, 1]
    @test output ≈ W * Float32[0, 1]
    @test pre ≈ Float32[0.9, 1]
    @test post ≈ Float32[0.9, 1]
    # Reflex boost scales BOTH directions; bounds still hold.
    update_readout!(W, pre, post, Float32[1, 0], output;
        decay=0.9f0, eta=100.0f0, w_max=1.0f0, learn=true)
    @test maximum(abs, W) <= 1.0f0
    @test any(W .== -1.0f0) && any(W .== 1.0f0)

    # Identical repeated simultaneous activity must not ratchet every row up.
    W = fill(0.2f0, 3, 2)
    pre, post, output = zeros(Float32, 2), zeros(Float32, 3), zeros(Float32, 3)
    for tick in 1:1000
        update_readout!(W, pre, post, ones(Float32, 2), output;
            decay=0.995f0, eta=0.001f0, w_max=1.0f0, learn=tick % 10 == 0)
    end
    @test W == fill(0.2f0, 3, 2)
end

@testset "NERO momentum survives changed relevance and repeated updates" begin
    nero = NeroOrchestrator()
    ensemble = (lobes=[(output=zeros(Float32, N_OUT), last_spike_rate=d)
                      for d in Float32[0.9, 0.1, 0.2, 0.3]],)
    @test nero.raw_scores !== nero.prev_relevance
    for tick in 1:4
        old = copy(nero.relevance)
        previous = copy(nero.prev_relevance)
        expected_momentum = abs.(old .- previous)
        @test update_relevance!(nero, ensemble)
        @test nero.prev_relevance == old
        @test nero.raw_scores ≈ NERO_ALPHA .* nero.spike_density .+
            NERO_BETA .* nero.surprise .+ NERO_GAMMA .* expected_momentum
        if tick == 2
            @test maximum(expected_momentum) > 0.01f0
            @test any(nero.raw_scores .> NERO_ALPHA .* nero.spike_density .+
                      NERO_BETA .* nero.surprise)
        end
        @test sum(nero.relevance) ≈ 1.0f0 atol=1f-5
        @test all(isfinite, nero.relevance)
    end
    reset_nero!(nero)
    @test all(iszero, nero.raw_scores)
    @test all(iszero, nero.inhibited_scores)
    @test nero.prev_relevance == nero.relevance == fill(0.25f0, 4)
end

@testset "Live seed parsing, logging and replay" begin
    log = IOBuffer()
    withenv("LIMEN_SEED" => "2443") do
        @test seed_live_rng!(; io=log) == 2443
    end
    first = (rand(Float32, 8), randn(Float32, 8), randperm(8))
    seed_live_rng!("2443"; io=devnull)
    @test first == (rand(Float32, 8), randn(Float32, 8), randperm(8))
    @test occursin("LIMEN_SEED=2443", String(take!(log)))
    withenv("LIMEN_SEED" => nothing) do
        @test seed_live_rng!(; io=devnull) == 42
    end
    for bad in ("", "-1", "abc", "1.5", "18446744073709551616")
        @test_throws ArgumentError seed_live_rng!(bad; io=devnull)
    end
end

@testset "Optional covariance and signed liquidity shocks" begin
    for paths in (0, -1, 1, 1000)
        calls = 0
        for tick in 1:1000
            if covariance_due(paths, tick, 50)
                calls += 4
            end
        end
        @test calls == (paths > 0 ? 80 : 0)
    end
    @test !covariance_due(1, 49, 50)
    @test strongest_reflex(-0.5f0, 0.01f0) == -0.5f0
    @test strongest_reflex(0.5f0, 0.01f0) == 0.5f0
    @test strongest_reflex(-0.2f0, 0.5f0) == 0.5f0
    @test strongest_reflex(-0.5f0, 0.5f0) == -0.5f0
    @test abs(strongest_reflex(-0.5f0, 0.0f0)) > 0.1f0
end
end
