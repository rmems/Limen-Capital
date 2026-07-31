# adapters.jl — Load probes + re-exports for Limen-Neural packages
#
# Prefer build_reservoir() from reservoir.jl for the hot path.
# This file remains for status checks and TemporalFocus probing.
# Packages resolve via brain/Project.toml [sources] (git+rev), not sibling clones.

include(joinpath(@__DIR__, "hft_inhibition.jl"))

function _try_use_liquid_cortex()
    try
        @eval using LiquidCortex
        return true
    catch e
        @debug "LiquidCortex not available via Pkg" exception = e
        return false
    end
end

function _try_use_temporal_focus()
    try
        @eval using TemporalFocus
        return true
    catch e
        @debug "TemporalFocus not available via Pkg" exception = e
        return false
    end
end

const HAS_LIQUID_CORTEX = _try_use_liquid_cortex()
const HAS_TEMPORAL_FOCUS = _try_use_temporal_focus()

function adapter_status()
    return (
        liquid_cortex=HAS_LIQUID_CORTEX,
        temporal_focus=HAS_TEMPORAL_FOCUS,
    )
end

println("[adapters] LiquidCortex=$(HAS_LIQUID_CORTEX) TemporalFocus=$(HAS_TEMPORAL_FOCUS)")
