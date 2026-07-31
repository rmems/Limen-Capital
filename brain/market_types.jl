# market_types.jl — MarketPulse wire types (no CUDA)
#
# Shared by market_encoder, naut_core, and unit tests.
# Layout: docs/wire-protocol-v1.md

# ── MarketPulse packed struct layout (120 bytes from Rust) ────────────────────
# Canonical channel order: DNX(0), Quai(1), Qubic(2), Kaspa(3), Monero(4), Ocean(5), Verus(6)

struct MarketPulse
    timestamp_ns::UInt64
    dnx_price::Float32
    dnx_vol::Float32
    quai_price::Float32
    quai_vol::Float32
    qubic_price::Float32
    qubic_vol::Float32
    kaspa_price::Float32
    kaspa_vol::Float32
    monero_price::Float32
    monero_vol::Float32
    ocean_price::Float32
    ocean_vol::Float32
    verus_price::Float32
    verus_vol::Float32
    confidence_signal::Float32
    funding_rate::Float32
    liquidation_vol::Float32
    liquidity_delta::Float32
    l3_order_imbalance::Float32
    gpu_temp_c::Float32
    gpu_power_w::Float32
    gpu_util_pct::Float32
    basys_buffer_load::Float32
    dydx_oi_delta::Float32
    dydx_funding_rate::Float32
end

"""
    validate_market_pulse_fields!(f::AbstractVector{Float32}) -> Vector{Float32}

Require all floats finite; reject non-positive prices; soft-clamp vols to [0, 1].
Layout of `f` matches decode: 7×(price, vol) + 11 trailing signals.
"""
function validate_market_pulse_fields(f::AbstractVector{Float32})
    length(f) == 25 || error("Expected 25 Float32 fields, got $(length(f))")
    for (i, x) in enumerate(f)
        isfinite(x) || error("MarketPulse field $i is not finite: $x")
    end
    # Prices at odd indices 1,3,5,...,13 must be > 0
    for i in 1:2:13
        f[i] > 0 || error("MarketPulse price at field $i must be > 0, got $(f[i])")
    end
    out = Vector{Float32}(undef, 25)
    copyto!(out, f)
    # Soft-clamp volumes (indices 2,4,...,14) into [0, 1]
    for i in 2:2:14
        out[i] = clamp(out[i], 0.0f0, 1.0f0)
    end
    return out
end

"""
    decode_market_pulse(buf::Vector{UInt8}) -> MarketPulse

Decode the 120-byte little-endian MarketPulse packet.
Bytes [108..120] are reserved (not decoded into fields today).

Rejects non-finite floats and non-positive prices; soft-clamps asset vols to [0, 1].
"""
function decode_market_pulse(buf::Vector{UInt8})
    length(buf) == 120 || error("Expected 120 bytes, got $(length(buf))")

    ts = reinterpret(UInt64, view(buf, 1:8))[1]
    raw = collect(reinterpret(Float32, view(buf, 9:108)))  # 25 Float32 values
    f = validate_market_pulse_fields(raw)

    return MarketPulse(
        ts,
        f[1], f[2],   # dnx
        f[3], f[4],   # quai
        f[5], f[6],   # qubic
        f[7], f[8],   # kaspa
        f[9], f[10],  # monero
        f[11], f[12], # ocean
        f[13], f[14], # verus
        f[15],        # confidence_signal
        f[16],        # funding_rate
        f[17],        # liquidation_vol
        f[18],        # liquidity_delta
        f[19],        # l3_order_imbalance
        f[20],        # gpu_temp_c
        f[21],        # gpu_power_w
        f[22],        # gpu_util_pct
        f[23],        # basys_buffer_load
        f[24],        # dydx_oi_delta
        f[25],        # dydx_funding_rate
    )
end

"""
    pack_market_pulse(pulse::MarketPulse) -> Vector{UInt8}

Encode MarketPulse to 120-byte buffer (for tests / mock publishers).
"""
function pack_market_pulse(pulse::MarketPulse)
    buf = zeros(UInt8, 120)
    copyto!(buf, 1, reinterpret(UInt8, [pulse.timestamp_ns]), 1, 8)
    floats = Float32[
        pulse.dnx_price, pulse.dnx_vol,
        pulse.quai_price, pulse.quai_vol,
        pulse.qubic_price, pulse.qubic_vol,
        pulse.kaspa_price, pulse.kaspa_vol,
        pulse.monero_price, pulse.monero_vol,
        pulse.ocean_price, pulse.ocean_vol,
        pulse.verus_price, pulse.verus_vol,
        pulse.confidence_signal,
        pulse.funding_rate, pulse.liquidation_vol,
        pulse.liquidity_delta, pulse.l3_order_imbalance,
        pulse.gpu_temp_c, pulse.gpu_power_w, pulse.gpu_util_pct,
        pulse.basys_buffer_load,
        pulse.dydx_oi_delta, pulse.dydx_funding_rate,
    ]
    copyto!(buf, 9, reinterpret(UInt8, floats), 1, 100)
    return buf
end

"""
    pack_readout(tick, output::Vector{Float32}, relevance::Vector{Float32}) -> Vector{UInt8}

88-byte ReadoutPacket: tick (i64) + 16×f32 readout + 4×f32 relevance.
"""
function pack_readout(tick::Int64, output::AbstractVector{Float32}, relevance::AbstractVector{Float32})
    length(output) == 16 || error("output must be length 16")
    length(relevance) == 4 || error("relevance must be length 4")
    buf = Vector{UInt8}(undef, 88)
    copyto!(buf, 1, reinterpret(UInt8, [tick]), 1, 8)
    copyto!(buf, 9, reinterpret(UInt8, collect(output)), 1, 64)
    copyto!(buf, 73, reinterpret(UInt8, collect(relevance)), 1, 16)
    return buf
end

"""
    decode_readout(buf::Vector{UInt8}) -> (tick, output, relevance)
"""
function decode_readout(buf::Vector{UInt8})
    length(buf) == 88 || error("Expected 88 bytes, got $(length(buf))")
    tick = reinterpret(Int64, view(buf, 1:8))[1]
    output = collect(reinterpret(Float32, view(buf, 9:72)))
    relevance = collect(reinterpret(Float32, view(buf, 73:88)))
    return tick, output, relevance
end
