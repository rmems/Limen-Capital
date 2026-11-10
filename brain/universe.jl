# universe.jl — runtime tradeable symbol table (must match execution/src/universe.rs)

import TOML

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
        for b in codeunits(sym)
            h = _fnv_byte(h, b)
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
    length(unique(symbols)) == length(symbols) || error("duplicate symbol in universe")
    TradeUniverse(symbols, symbol_table_hash(symbols))
end

function _normalize_symbols(raw)
    raw isa AbstractVector || error("universe.toml `symbols` must be an array")
    syms = String[]
    for entry in raw
        entry isa AbstractString || error("each symbol must be a string")
        s = strip(String(entry))
        isempty(s) && error("empty symbol in universe")
        push!(syms, s)
    end
    return syms
end

function _parse_universe_toml(text::AbstractString)
    doc = TOML.parse(text)
    haskey(doc, "symbols") || error("universe.toml missing `symbols` array")
    return TradeUniverse(_normalize_symbols(doc["symbols"]))
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

"""Checked-in `config/universe.toml`, ignoring `LIMEN_UNIVERSE`."""
function canonical_universe_hash()
    path = abspath(joinpath(@__DIR__, "..", "config", "universe.toml"))
    return _parse_universe_toml(read(path, String)).hash
end
