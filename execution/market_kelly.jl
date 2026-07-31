#!/usr/bin/env julia
# market_kelly.jl — Thin CLI for the same Kelly formula as execution/src/kelly.rs
#
# Canonical sizing lives in Rust `PositionSizer` (ExecutionEngine Decision path).
# This script is for offline / shell tooling only — do not fork a third formula.
#
# Full Kelly:  f* = (p·b − q) / b
# Fractional:  f  = f* × FRAC   (default half-Kelly 0.5 for this CLI; Rust uses 0.25)
# Clamp:       [0.02, 0.20] for shell output stability
#
# Usage:
#   SHIP_WIN_RATE=0.55 SHIP_AVG_WIN=8.50 SHIP_AVG_LOSS=5.20 julia market_kelly.jl
#   # or payoff ratio form:
#   SHIP_WIN_RATE=0.98 SHIP_PAYOFF_B=0.05 SHIP_FRAC=0.25 julia market_kelly.jl

using Printf

function kelly_fraction(win_rate::Float64, b::Float64, frac::Float64)::Float64
    p = clamp(win_rate, 0.01, 0.99)
    b = max(b, 1e-9)
    q = 1.0 - p
    full = (p * b - q) / b
    return max(0.0, full) * frac
end

function main()
    win_rate = parse(Float64, get(ENV, "SHIP_WIN_RATE", "0.0"))
    avg_win = parse(Float64, get(ENV, "SHIP_AVG_WIN", "0.0"))
    avg_loss = parse(Float64, get(ENV, "SHIP_AVG_LOSS", "0.0"))
    frac = parse(Float64, get(ENV, "SHIP_FRAC", "0.5"))

    b = if haskey(ENV, "SHIP_PAYOFF_B")
        parse(Float64, ENV["SHIP_PAYOFF_B"])
    elseif avg_win > 0 && avg_loss > 0
        avg_win / avg_loss
    else
        0.0
    end

    if win_rate < 0.01 || b < 1e-9
        println("0.05")  # conservative floor when under-specified
        return
    end

    f = kelly_fraction(win_rate, b, frac)
    # Shell clamp for pilot scripts; Rust engine does not use this clamp on ATP path
    f = clamp(f, 0.0, 1.0)
    if get(ENV, "SHIP_CLAMP_PILOT", "1") == "1"
        f = clamp(f, 0.02, 0.20)
    end
    @printf("%.4f\n", f)
end

main()
