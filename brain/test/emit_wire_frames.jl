# CPU-only producer for the required Julia → Rust decision test.
include(joinpath(@__DIR__, "..", "market_types.jl"))
length(ARGS) == 1 || error("usage: emit_wire_frames.jl OUTPUT_DIR")
dir = only(ARGS)
mkpath(dir)
N_ASSETS == 7 || error("cross-language test requires the canonical universe")
ts = UInt64(1_700_000_000_000_000_001)
pulse = MarketPulse(; timestamp_ns=ts,
                    prices=Float32[100, 200, 300, 400, 500, 600, 700],
                    vols=Float32[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7])
write(joinpath(dir, "market.bin"), pack_market_pulse(pulse))
for (name, bull, bear, relevance) in (
    ("buy", 3.0f0, 0.0f0, Float32[1, 0, 0, 0]),
    ("sell", 0.0f0, 3.0f0, Float32[1, 0, 0, 0]),
    ("neutral", 0.0f0, 0.0f0, Float32[1, 0, 0, 0]),
    ("low_confidence", 0.1f0, 0.0f0, Float32[1, 0, 0, 0]),
    ("no_edge", 0.8f0, 0.1f0, Float32[0.4, 0.3, 0.2, 0.1]),
)
    output = zeros(Float32, N_OUT)
    output[1:2] .= (bull, bear)
    write(joinpath(dir, "$name.bin"), pack_readout(Int64(ts), output, relevance))
end
