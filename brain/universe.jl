# universe.jl — runtime tradeable symbol table (must match execution/src/universe.rs)

const FNV_OFFSET = UInt64(14695981039346656037)
const FNV_PRIME = UInt64(1099511628211)

function _fnv_byte(h::UInt64, b::UInt8)::UInt64
    h = xor(h, UInt64(b))
    return h * FNV_PRIME
end

"""
    symbol_table_hash(symbols::AbstractVector{<:AbstractString}) -> UInt64

FNV-1a over tickers joined by newline (same algorithm as Rust muscle).
"""
function symbol_table_hash(symbols::AbstractVector{<:AbstractString})
    h = FNV_OFFSET
    for (i, sym) in enumerate(symbols)
        if i > 1
            h = _fnv_byte(h, UInt8('\n'))
        end
        for c in sym
            h = _fnv_byte(h, UInt8(c))
        end
    end
    return h
end

struct TradeUniverse
    symbols::Vector{String}
    hash::UInt64
end

function TradeUniverse(symbols::Vector{String})
    isempty(symbols) && error("universe must contain at least one symbol")
    length(symbols) > 256 && error("universe too large: $(length(symbols)) symbols")
    TradeUniverse(symbols, symbol_table_hash(symbols))
end

function _parse_universe_toml(text::AbstractString)
    m = match(r"symbols\s*=\s*\[(.*?)\]"s, text)
    m === nothing && error("universe.toml missing `symbols` array")
    body = strip(m.captures[1])
    syms = String[]
    for chunk in eachmatch(r"\"([^\"]+)\""s, body)
        push!(syms, chunk.captures[1])
    end
    isempty(syms) && error("no symbols parsed from universe file")
    return TradeUniverse(syms)
end

"""
    load_trade_universe() -> TradeUniverse

Default: `config/universe.toml` under repo root, or `LIMEN_UNIVERSE` override.
"""
function load_trade_universe()
    path = if haskey(ENV, "LIMEN_UNIVERSE") && !isempty(ENV["LIMEN_UNIVERSE"])
        p = ENV["LIMEN_UNIVERSE"]
        isabspath(p) ? p : abspath(joinpath(@__DIR__, "..", p))
    else
        abspath(joinpath(@__DIR__, "..", "config", "universe.toml"))
    end
    text = read(path, String)
    return _parse_universe_toml(text)
end
