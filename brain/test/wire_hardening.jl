@testset "Network envelope" begin
    for (topic, name, size) in (("NW1P", "marketpulse", 120), ("NW1R", "readout", 88))
        raw = read(joinpath(BRAIN, "..", "wire", "fixtures", "$name.bin"))
        framed = read(joinpath(BRAIN, "..", "wire", "fixtures", "$name.frame"))
        @test frame_payload(topic, raw) == framed
        @test unframe_payload(framed, topic, size) == raw
        @test_throws ErrorException unframe_payload(raw, topic, size)
        @test_throws ErrorException unframe_payload(UInt8[], topic, size)
        @test_throws ErrorException unframe_payload(framed[1:end-1], topic, size)
        for offset in 1:8
            bad = copy(framed)
            bad[offset] ⊻= 0xff
            @test_throws ErrorException unframe_payload(bad, topic, size)
        end
    end
end

@testset "Binary endpoint policy" begin
    for ep in ("tcp://127.0.0.1:5555", "tcp://127.42.0.1:5556", "tcp://[::1]:5555", "ipc:///tmp/wire.sock")
        @test validate_binary_endpoint(ep; allow_remote=false) == ep
    end
    for ep in ("tcp://0.0.0.0:5555", "tcp://*:5555", "tcp://[::]:5555", "tcp://192.0.2.1:5555", "tcp://localhost:5555", "tcp://example.org:5555")
        @test_throws ErrorException validate_binary_endpoint(ep; allow_remote=false)
        @test validate_binary_endpoint(ep; allow_remote=true) == ep
    end
    for ep in ("", "udp://127.0.0.1:5555", "ipc://relative", "ipc:///tmp/../wire", "tcp://127.0.0.1:0", "tcp://127.0.0.1:65536")
        @test_throws ErrorException validate_binary_endpoint(ep; allow_remote=false)
        @test_throws ErrorException validate_binary_endpoint(ep; allow_remote=true)
    end
end

@testset "Readout finite guard" begin
    for bad in (NaN32, Inf32, -Inf32)
        for index in 1:16
            output = zeros(Float32, 16)
            output[index] = bad
            @test_throws ErrorException pack_readout(Int64(1), output, fill(0.25f0, 4))
        end
        for index in 1:4
            relevance = fill(0.25f0, 4)
            relevance[index] = bad
            @test_throws ErrorException pack_readout(Int64(1), zeros(Float32, 16), relevance)
        end
    end
    @test_throws ErrorException pack_readout(Int64(1), zeros(Float32, 16), ones(Float32, 4))
    bad = read(joinpath(BRAIN, "..", "wire", "fixtures", "marketpulse.bin"))
    bad[end] = 1
    @test_throws ErrorException decode_market_pulse(bad)
end

# Load the real NERO implementation without a GPU, not a copy of its equations.
module NeroRecoveryTests
using Test, LinearAlgebra, Printf
const N_LOBES = 4
const N_OUT = 16
include(joinpath(@__DIR__, "..", "synapse_conductor.jl"))

function healthy_ensemble()
    return (lobes=[(output=fill(0.1f0, 16), last_spike_rate=0.1f0) for _ in 1:4],
            agg_output=zeros(Float32, 16), weights=fill(0.25f0, 4))
end

@testset "NERO finite recovery" begin
    withenv("LIMEN_AGG_MODE" => "relevance") do
        for bad in (NaN32, Inf32, -Inf32, floatmax(Float32))
            nero = NeroOrchestrator()
            ensemble = healthy_ensemble()
            fill!(ensemble.lobes[1].output, bad)
            @test route_and_aggregate!(nero, ensemble) === nothing
            @test nero_finite(nero)
            @test nero.relevance == fill(0.25f0, 4)
            fill!(ensemble.lobes[1].output, 0.1f0)
            for _ in 1:3
                @test all(isfinite, route_and_aggregate!(nero, ensemble))
                @test sum(nero.relevance) ≈ 1.0f0 atol=1f-5
            end
        end
        for field in (:relevance, :prev_relevance, :readout_ema, :spike_density, :temp_buffer, :surprise)
            nero = NeroOrchestrator()
            getfield(nero, field)[1] = NaN32
            @test all(isfinite, route_and_aggregate!(nero, healthy_ensemble()))
            @test nero_finite(nero)
        end
        nero = NeroOrchestrator()
        ensemble = healthy_ensemble()
        ensemble.lobes[1] = (output=zeros(Float32, 16), last_spike_rate=NaN32)
        @test route_and_aggregate!(nero, ensemble) === nothing
        @test nero_finite(nero)
    end
end
end
