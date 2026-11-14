# CPU-testable NERO state, routing, and aggregation shared with the unit suite.
include(joinpath(@__DIR__, "nero_core.jl"))

# ── Diagnostics ───────────────────────────────────────────────────────────────

"""
    nero_diagnostics(nero::NeroOrchestrator) -> String

One-line NERO state summary for the diagnostic loop.
"""
function nero_diagnostics(nero::NeroOrchestrator)::String
    lobe_strs = String[]
    for i in 1:N_LOBES
        push!(lobe_strs, @sprintf("%s=%.2f", LOBE_NAMES[i], nero.relevance[i]))
    end
    dominant = argmax(nero.relevance)
    @sprintf("[NERO tick=%d] %s | dominant=%s | surprise=[%.3f,%.3f,%.3f,%.3f] | agg=causal",
        nero.tick_count,
        join(lobe_strs, " "),
        LOBE_NAMES[dominant],
        nero.surprise[1], nero.surprise[2], nero.surprise[3], nero.surprise[4])
end

println("[nero] nero_orchestrator.jl loaded — NERO graph ($(N_LOBES) lobes) + causal aggregation")
