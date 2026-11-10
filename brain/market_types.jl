# market_types.jl — MarketPulse wire types (NervousWire v2 + legacy v1)

using Sockets: IPAddr, IPv4, IPv6

include(joinpath(@__DIR__, "universe.jl"))

const TRADE_UNIVERSE = load_trade_universe()
const N_ASSETS = length(TRADE_UNIVERSE.symbols)
const N_PAIRS = N_ASSETS + 1
const N_IN = 4 * N_ASSETS
const N_OUT = 2 * N_PAIRS

const WIRE_MAGIC = 0x56524e4c  # "LNRV" LE
const WIRE_VERSION_V2 = UInt8(2)
const FRAME_MARKET_PULSE = UInt8(1)
const FRAME_READOUT = UInt8(2)
const WIRE_HEADER_BYTES = 8
const MARKET_PULSE_V1_BYTES = 120
const READOUT_PACKET_V1_BYTES = 88
const TRAILING_SIGNAL_COUNT = 11

struct MarketPulse
    timestamp_ns::UInt64
    symbol_table_hash::UInt64
    prices::Vector{Float32}
    vols::Vector{Float32}
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

function MarketPulse(;
    timestamp_ns::UInt64 = 0,
    prices::AbstractVector{Float32},
    vols::AbstractVector{Float32},
    confidence_signal::Float32 = 0.0f0,
    funding_rate::Float32 = 0.0f0,
    liquidation_vol::Float32 = 0.0f0,
    liquidity_delta::Float32 = 0.0f0,
    l3_order_imbalance::Float32 = 0.0f0,
    gpu_temp_c::Float32 = 40.0f0,
    gpu_power_w::Float32 = 0.0f0,
    gpu_util_pct::Float32 = 0.0f0,
    basys_buffer_load::Float32 = 0.0f0,
    dydx_oi_delta::Float32 = 0.0f0,
    dydx_funding_rate::Float32 = 0.0f0,
)
    length(prices) == length(vols) == N_ASSETS ||
        error("expected $N_ASSETS assets, got $(length(prices)) prices")
    MarketPulse(
        timestamp_ns,
        TRADE_UNIVERSE.hash,
        collect(Float32, prices),
        collect(Float32, vols),
        confidence_signal,
        funding_rate,
        liquidation_vol,
        liquidity_delta,
        l3_order_imbalance,
        gpu_temp_c,
        gpu_power_w,
        gpu_util_pct,
        basys_buffer_load,
        dydx_oi_delta,
        dydx_funding_rate,
    )
end

function trailing_fields(p::MarketPulse)
    (
        p.confidence_signal,
        p.funding_rate,
        p.liquidation_vol,
        p.liquidity_delta,
        p.l3_order_imbalance,
        p.gpu_temp_c,
        p.gpu_power_w,
        p.gpu_util_pct,
        p.basys_buffer_load,
        p.dydx_oi_delta,
        p.dydx_funding_rate,
    )
end

function validate_market_pulse_fields!(prices::AbstractVector{Float32}, vols::AbstractVector{Float32}, trailing::AbstractVector{Float32})
    length(prices) == length(vols) || error("prices/vols length mismatch")
    for (i, x) in enumerate(prices)
        isfinite(x) || error("MarketPulse price[$i] is not finite: $x")
        x > 0 || error("MarketPulse price[$i] must be > 0, got $x")
    end
    for (i, v) in enumerate(vols)
        isfinite(v) || error("MarketPulse vol[$i] is not finite: $v")
        vols[i] = clamp(v, 0.0f0, 1.0f0)
    end
    for (i, x) in enumerate(trailing)
        isfinite(x) || error("MarketPulse trailing field $i is not finite: $x")
    end
    return nothing
end

function _wire_magic(buf::Vector{UInt8})
    length(buf) < 4 && return UInt32(0)
    return reinterpret(UInt32, view(buf, 1:4))[1]
end

function _require_v2_header(buf::Vector{UInt8}, frame_type::UInt8, label::String)
    length(buf) >= WIRE_HEADER_BYTES || error("$label frame shorter than header")
    _wire_magic(buf) == WIRE_MAGIC || error("invalid wire magic")
    buf[5] == WIRE_VERSION_V2 || error("unsupported wire version $(buf[5])")
    buf[6] == frame_type || error("expected $label frame, got type $(buf[6])")
    buf[7] == 0x00 && buf[8] == 0x00 || error("reserved wire header bytes must be zero")
    return nothing
end

# Bytes 27–28 (1-based) follow the u16 count in both v2 frame prefixes.
function _require_reserved_prefix(buf::Vector{UInt8})
    length(buf) >= 28 || error("frame shorter than v2 prefix")
    buf[27] == 0x00 && buf[28] == 0x00 || error("reserved wire prefix bytes must be zero")
    return nothing
end

function market_pulse_v2_len(n_assets::Int)
    WIRE_HEADER_BYTES + 8 + 8 + 4 + 8 * n_assets + TRAILING_SIGNAL_COUNT * 4
end

function pack_market_pulse_v2(pulse::MarketPulse)
    n = length(pulse.prices)
    buf = Vector{UInt8}(undef, market_pulse_v2_len(n))
    off = 1
    copyto!(buf, off, reinterpret(UInt8, [WIRE_MAGIC]), 1, 4); off += 4
    buf[off] = WIRE_VERSION_V2; off += 1
    buf[off] = FRAME_MARKET_PULSE; off += 1
    buf[off:off+1] .= 0x00; off += 2
    copyto!(buf, off, reinterpret(UInt8, [pulse.timestamp_ns]), 1, 8); off += 8
    copyto!(buf, off, reinterpret(UInt8, [pulse.symbol_table_hash]), 1, 8); off += 8
    copyto!(buf, off, reinterpret(UInt8, [UInt16(n)]), 1, 2); off += 2
    copyto!(buf, off, reinterpret(UInt8, [UInt16(0)]), 1, 2); off += 2
    asset_floats = Float32[]
    for i in 1:n
        push!(asset_floats, pulse.prices[i], pulse.vols[i])
    end
    copyto!(buf, off, reinterpret(UInt8, asset_floats), 1, 8 * n); off += 8 * n
    trailing = collect(Float32, trailing_fields(pulse))
    copyto!(buf, off, reinterpret(UInt8, trailing), 1, length(trailing) * 4)
    return buf
end

function decode_market_pulse_v2(buf::Vector{UInt8})
    _require_v2_header(buf, FRAME_MARKET_PULSE, "MarketPulse")
    _require_reserved_prefix(buf)
    off = WIRE_HEADER_BYTES + 1
    ts = reinterpret(UInt64, view(buf, off:off+7))[1]; off += 8
    hash = reinterpret(UInt64, view(buf, off:off+7))[1]; off += 8
    n = Int(reinterpret(UInt16, view(buf, off:off+1))[1]); off += 4
    hash == TRADE_UNIVERSE.hash ||
        error("symbol_table_hash mismatch (wire 0x$(string(hash, base=16)) vs universe 0x$(string(TRADE_UNIVERSE.hash, base=16)))")
    n == N_ASSETS || error("n_assets $n != universe size $N_ASSETS")
    expected = market_pulse_v2_len(n)
    length(buf) == expected || error("Expected $expected bytes, got $(length(buf))")
    asset_words = collect(reinterpret(Float32, view(buf, off:off + 8 * n - 1)))
    prices = asset_words[1:2:end]
    vols = asset_words[2:2:end]
    off += 8 * n
    trailing = collect(reinterpret(Float32, view(buf, off:off + TRAILING_SIGNAL_COUNT * 4 - 1)))
    validate_market_pulse_fields!(prices, vols, trailing)
    return MarketPulse(;
        timestamp_ns = ts,
        prices,
        vols,
        confidence_signal = trailing[1],
        funding_rate = trailing[2],
        liquidation_vol = trailing[3],
        liquidity_delta = trailing[4],
        l3_order_imbalance = trailing[5],
        gpu_temp_c = trailing[6],
        gpu_power_w = trailing[7],
        gpu_util_pct = trailing[8],
        basys_buffer_load = trailing[9],
        dydx_oi_delta = trailing[10],
        dydx_funding_rate = trailing[11],
    )
end

function pack_market_pulse_v1(pulse::MarketPulse)
    N_ASSETS == 7 || error("v1 pack requires 7-symbol universe")
    buf = zeros(UInt8, MARKET_PULSE_V1_BYTES)
    copyto!(buf, 1, reinterpret(UInt8, [pulse.timestamp_ns]), 1, 8)
    floats = Float32[]
    for i in 1:N_ASSETS
        push!(floats, pulse.prices[i], pulse.vols[i])
    end
    append!(floats, collect(Float32, trailing_fields(pulse)))
    copyto!(buf, 9, reinterpret(UInt8, floats), 1, length(floats) * 4)
    return buf
end

function decode_market_pulse_v1(buf::Vector{UInt8})
    length(buf) == MARKET_PULSE_V1_BYTES || error("Expected 120 bytes, got $(length(buf))")
    N_ASSETS == 7 || error("v1 decode requires 7-symbol universe")
    TRADE_UNIVERSE.hash == canonical_universe_hash() ||
        error("v1 frames are only valid for the checked-in default universe")
    ts = reinterpret(UInt64, view(buf, 1:8))[1]
    words = collect(reinterpret(Float32, view(buf, 9:108)))
    prices = words[1:2:13]
    vols = words[2:2:14]
    trailing = words[15:25]
    validate_market_pulse_fields!(prices, vols, trailing)
    return MarketPulse(;
        timestamp_ns = ts,
        prices,
        vols,
        confidence_signal = trailing[1],
        funding_rate = trailing[2],
        liquidation_vol = trailing[3],
        liquidity_delta = trailing[4],
        l3_order_imbalance = trailing[5],
        gpu_temp_c = trailing[6],
        gpu_power_w = trailing[7],
        gpu_util_pct = trailing[8],
        basys_buffer_load = trailing[9],
        dydx_oi_delta = trailing[10],
        dydx_funding_rate = trailing[11],
    )
end

function decode_market_pulse(buf::Vector{UInt8})
    if length(buf) >= WIRE_HEADER_BYTES && _wire_magic(buf) == WIRE_MAGIC
        return decode_market_pulse_v2(buf)
    elseif length(buf) == MARKET_PULSE_V1_BYTES
        return decode_market_pulse_v1(buf)
    end
    error("unrecognized MarketPulse frame ($(length(buf)) bytes)")
end

pack_market_pulse(p::MarketPulse) = pack_market_pulse_v2(p)

function readout_frame_v2_len(n_pairs::Int)
    WIRE_HEADER_BYTES + 8 + 8 + 4 + 4 * (2 * n_pairs) + 16
end

"""
    validate_readout_values(output, relevance)

Refuse to publish what the Rust decoder would reject: non-finite readout or
relevance, relevance outside [0, 1], or relevance not summing to 1 ± 0.05.
"""
function validate_readout_values(output::AbstractVector{Float32}, relevance::AbstractVector{Float32})
    length(relevance) == 4 || error("relevance must be length 4")
    for (i, x) in enumerate(output)
        isfinite(x) || error("readout output[$i] is not finite: $x")
    end
    for (i, r) in enumerate(relevance)
        isfinite(r) || error("readout relevance[$i] is not finite: $r")
        0.0f0 <= r <= 1.0f0 || error("readout relevance[$i] must be in [0, 1], got $r")
    end
    s = sum(relevance)
    0.95f0 <= s <= 1.05f0 || error("readout relevance must sum to ~1.0 (±0.05), got $s")
    return nothing
end

function pack_readout(tick::Int64, output::AbstractVector{Float32}, relevance::AbstractVector{Float32})
    validate_readout_values(output, relevance)
    n_pairs = length(output) ÷ 2
    n_pairs == N_PAIRS || error("output length $(length(output)) != 2×$N_PAIRS")
    buf = Vector{UInt8}(undef, readout_frame_v2_len(n_pairs))
    off = 1
    copyto!(buf, off, reinterpret(UInt8, [WIRE_MAGIC]), 1, 4); off += 4
    buf[off] = WIRE_VERSION_V2; off += 1
    buf[off] = FRAME_READOUT; off += 1
    buf[off:off+1] .= 0x00; off += 2
    copyto!(buf, off, reinterpret(UInt8, [tick]), 1, 8); off += 8
    copyto!(buf, off, reinterpret(UInt8, [TRADE_UNIVERSE.hash]), 1, 8); off += 8
    copyto!(buf, off, reinterpret(UInt8, [UInt16(n_pairs)]), 1, 2); off += 2
    copyto!(buf, off, reinterpret(UInt8, [UInt16(0)]), 1, 2); off += 2
    copyto!(buf, off, reinterpret(UInt8, collect(output)), 1, length(output) * 4); off += length(output) * 4
    copyto!(buf, off, reinterpret(UInt8, collect(relevance)), 1, 16)
    return buf
end

function decode_readout_v2(buf::Vector{UInt8})
    _require_v2_header(buf, FRAME_READOUT, "Readout")
    _require_reserved_prefix(buf)
    off = WIRE_HEADER_BYTES + 1
    tick = reinterpret(Int64, view(buf, off:off+7))[1]; off += 8
    hash = reinterpret(UInt64, view(buf, off:off+7))[1]; off += 8
    hash == TRADE_UNIVERSE.hash || error("readout symbol_table_hash mismatch")
    n_pairs = Int(reinterpret(UInt16, view(buf, off:off+1))[1]); off += 4
    n_pairs == N_PAIRS || error("n_pairs $n_pairs != $N_PAIRS")
    expected = readout_frame_v2_len(n_pairs)
    length(buf) == expected || error("Expected $expected bytes, got $(length(buf))")
    n_out = 2 * n_pairs
    output = collect(reinterpret(Float32, view(buf, off:off + n_out * 4 - 1)))
    off += n_out * 4
    relevance = collect(reinterpret(Float32, view(buf, off:off + 15)))
    return tick, output, relevance
end

function decode_readout_v1(buf::Vector{UInt8})
    length(buf) == READOUT_PACKET_V1_BYTES || error("Expected 88 bytes, got $(length(buf))")
    N_PAIRS == 8 || error("v1 readout requires 8-pair universe")
    TRADE_UNIVERSE.hash == canonical_universe_hash() ||
        error("v1 frames are only valid for the checked-in default universe")
    tick = reinterpret(Int64, view(buf, 1:8))[1]
    output = collect(reinterpret(Float32, view(buf, 9:72)))
    relevance = collect(reinterpret(Float32, view(buf, 73:88)))
    return tick, output, relevance
end

function decode_readout(buf::Vector{UInt8})
    if length(buf) >= WIRE_HEADER_BYTES && _wire_magic(buf) == WIRE_MAGIC
        return decode_readout_v2(buf)
    elseif length(buf) == READOUT_PACKET_V1_BYTES
        return decode_readout_v1(buf)
    end
    error("unrecognized Readout frame ($(length(buf)) bytes)")
end

function classify_wire_frame(buf::Vector{UInt8})
    if length(buf) >= WIRE_HEADER_BYTES && _wire_magic(buf) == WIRE_MAGIC
        buf[6] == FRAME_MARKET_PULSE && return :market_v2
        buf[6] == FRAME_READOUT && return :readout_v2
        return :unknown
    elseif length(buf) == MARKET_PULSE_V1_BYTES
        return :market_v1
    elseif length(buf) == READOUT_PACKET_V1_BYTES
        return :readout_v1
    end
    return :unknown
end

# ── Endpoint policy and source timestamps ─────────────────────────────────────

"""Wall-clock nanoseconds since the Unix epoch (`time_ns()` is monotonic, not epoch)."""
function wall_clock_ns()
    tv = Libc.TimeVal()
    return UInt64(tv.sec) * UInt64(1_000_000_000) + UInt64(tv.usec) * UInt64(1_000)
end

_is_loopback(ip::IPv4) = (ip.host >> 24) == 127
_is_loopback(ip::IPv6) = ip == IPv6(1)

function _loopback_literal(host::AbstractString)
    bare = strip(host, ['[', ']'])
    ip = try
        parse(IPAddr, bare)
    catch e
        e isa ArgumentError || rethrow()
        return false
    end
    return _is_loopback(ip)
end

function _validate_ipc_endpoint(ep::AbstractString)
    path = ep[length("ipc://")+1:end]
    startswith(path, "/") || error("ipc:// endpoint path must be absolute")
    any(==(".."), split(path, '/')) && error("ipc:// endpoint must not contain '..' segments")
    return String(ep)
end

"""
    validate_binary_endpoint(ep; allow_remote=ENV["LIMEN_ALLOW_REMOTE_WIRE"] == "1")

Accept `ipc://` absolute paths and `tcp://` literal loopback addresses with a
port in 1..65535. Hostnames are not resolved, so `localhost` counts as remote.
"""
function validate_binary_endpoint(ep::AbstractString;
                                  allow_remote::Bool = get(ENV, "LIMEN_ALLOW_REMOTE_WIRE", "") == "1")
    startswith(ep, "ipc://") && return _validate_ipc_endpoint(ep)
    startswith(ep, "tcp://") || error("binary endpoint must use tcp:// or ipc://")
    addr = ep[length("tcp://")+1:end]
    idx = findlast(':', addr)
    idx === nothing && error("TCP endpoint requires a port")
    port = tryparse(Int, addr[idx+1:end])
    port !== nothing && 1 <= port <= 65535 || error("TCP endpoint requires a port in 1..65535")
    allow_remote || _loopback_literal(addr[1:idx-1]) ||
        error("non-loopback wire endpoint requires LIMEN_ALLOW_REMOTE_WIRE=1 (use CURVE on untrusted networks)")
    return String(ep)
end

"""Per-stream watermark seeded at startup; refuses duplicate, older, and pre-restart timestamps."""
mutable struct TimestampGuard
    last_ns::UInt64
end

function accept_timestamp!(guard::TimestampGuard, ts::UInt64)
    ts > guard.last_ns || return false
    guard.last_ns = ts
    return true
end
