# Literal values are independent of both decoders and the Python struct oracle.
@testset "Committed wire fixtures pin writers and all decoded fields" begin
    @test N_ASSETS == 7
    pulse = MarketPulse(;
        timestamp_ns=UInt64(1_700_000_000_000_000_001),
        prices=Float32[1, 1.1, 1.2, 1.3, 1.4, 1.5, 1.6],
        vols=Float32[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
        confidence_signal=0.9f0, funding_rate=0.0001f0,
        liquidation_vol=1_000_000.0f0, liquidity_delta=0.05f0,
        l3_order_imbalance=0.1f0, gpu_temp_c=45.0f0, gpu_power_w=200.0f0,
        gpu_util_pct=0.5f0, basys_buffer_load=0.1f0,
        dydx_oi_delta=0.01f0, dydx_funding_rate=0.0002f0)
    fixtures = joinpath(BRAIN, "..", "wire", "fixtures")
    for (name, packer) in (("marketpulse.bin", pack_market_pulse_v1),
                           ("marketpulse_v2.bin", pack_market_pulse))
        bytes = read(joinpath(fixtures, name)) # Missing fixtures must fail.
        @test packer(pulse) == bytes
        decoded = decode_market_pulse(bytes)
        for field in fieldnames(MarketPulse)
            @test getfield(decoded, field) == getfield(pulse, field)
        end
    end
    relevance = Float32[0.4, 0.3, 0.2, 0.1]
    legacy = vcat(Float32[0.8, 0.1, 0.2, 0.3], zeros(Float32, 12))
    output = vcat(Float32[0.8, 0.1, 0.2, 0.3], Float32[i / 100 for i in 4:15])
    for (name, expected) in (("readout.bin", legacy), ("readout_v2.bin", output))
        bytes = read(joinpath(fixtures, name))
        tick, decoded, rel = decode_readout(bytes)
        @test tick == 42
        @test decoded == expected
        @test rel == relevance
    end
    @test pack_readout(Int64(42), output, relevance) ==
          read(joinpath(fixtures, "readout_v2.bin"))
end
