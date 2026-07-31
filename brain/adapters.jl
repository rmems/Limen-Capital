# adapters.jl — Load probes + re-exports for Limen-Neural packages
#
# Prefer build_reservoir() from reservoir.jl for the hot path.
# This file remains for status checks and TemporalFocus probing.

include(joinpath(@__DIR__, "hft_inhibition.jl"))

const _LIMEN_NEURAL = get(ENV, "LIMEN_NEURAL",
    abspath(joinpath(@__DIR__, "..", "..", "Limen-Neural")))

function _try_use_liquid_cortex()
    try
        @eval using LiquidCortex
        return true
    catch
        lc = joinpath(_LIMEN_NEURAL, "LiquidCortex.jl")
        if isdir(lc)
            try
                if !(lc in LOAD_PATH)
                    push!(LOAD_PATH, lc)
                end
                @eval using LiquidCortex
                return true
            catch e
                @warn "LiquidCortex path present but failed to load" exception=e
            end
        end
        return false
    end
end

function _try_use_temporal_focus()
    try
        @eval using TemporalFocus
        return true
    catch
        np = joinpath(_LIMEN_NEURAL, "NeuroPulse.jl")
        if isdir(np)
            try
                if !(np in LOAD_PATH)
                    push!(LOAD_PATH, np)
                end
                @eval using TemporalFocus
                return true
            catch e
                @warn "TemporalFocus path present but failed to load" exception=e
            end
        end
        return false
    end
end

const HAS_LIQUID_CORTEX = _try_use_liquid_cortex()
const HAS_TEMPORAL_FOCUS = _try_use_temporal_focus()

function adapter_status()
    return (
        liquid_cortex=HAS_LIQUID_CORTEX,
        temporal_focus=HAS_TEMPORAL_FOCUS,
        limen_neural=_LIMEN_NEURAL,
    )
end

println("[adapters] LiquidCortex=$(HAS_LIQUID_CORTEX) TemporalFocus=$(HAS_TEMPORAL_FOCUS)")
