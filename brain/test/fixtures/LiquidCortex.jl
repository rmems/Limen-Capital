# CPU-only API double for fresh-process world-age tests, not GPU qualification.
module LiquidCortex
export EnsembleBrain

mutable struct EnsembleBrain
    ticks::Int
    output::Vector{Float32}
end
EnsembleBrain(; n_in, n_out) = EnsembleBrain(0, zeros(Float32, n_out))
function ensemble_step!(ens::EnsembleBrain, u; inhibition, reflex_signal)
    ens.ticks += 1
    ens.output .= sum(u) - inhibition + reflex_signal
end
get_ensemble_output(ens::EnsembleBrain) = copy(ens.output)
ensemble_diagnostics(ens::EnsembleBrain) = (ticks=ens.ticks,)
end
