# reservoir.jl — Deep Reservoir façade (Capital)
#
# Single interface for helms:
#   res = build_reservoir()
#   reservoir_step!(res, u_gpu; pulse=... or stress kwargs)
#   y = reservoir_output(res)
#
# Backends:
#   :liquid_cortex — Limen-Neural LiquidCortex.jl (preferred when loadable)
#   :naut_core     — local brain/naut_core.jl EnsembleBrain
#
# Env: LIMEN_RESERVOIR=auto|liquid_cortex|naut_core  (default auto)

if !@isdefined(hft_inhibition)
    include(joinpath(@__DIR__, "hft_inhibition.jl"))
end

const _LIMEN_NEURAL = get(ENV, "LIMEN_NEURAL",
    abspath(joinpath(@__DIR__, "..", "..", "Limen-Neural")))

function _try_load_liquid_cortex()::Bool
    try
        @eval Main using LiquidCortex
        return true
    catch
        lc = joinpath(_LIMEN_NEURAL, "LiquidCortex.jl")
        if isdir(lc)
            try
                if !(lc in LOAD_PATH)
                    push!(LOAD_PATH, lc)
                end
                @eval Main using LiquidCortex
                return true
            catch e
                @warn "LiquidCortex path present but failed to load" exception = e
            end
        end
        return false
    end
end

"""
    Reservoir

Backend-agnostic handle. `ensemble` is LiquidCortex or naut_core EnsembleBrain.
"""
mutable struct Reservoir
    backend::Symbol
    ensemble::Any
    n_in::Int
    n_out::Int
end

"""
    build_reservoir(; backend=:auto, n_in=28, n_out=16) -> Reservoir

Construct a 4-lobe ensemble. Prefer LiquidCortex when available and backend is `:auto`.
"""
function build_reservoir(; backend::Symbol=:auto, n_in::Int=28, n_out::Int=16)
    pref = Symbol(get(ENV, "LIMEN_RESERVOIR", String(backend)))
    if pref === :auto
        pref = :auto
    end

    use_lc = false
    if pref === :liquid_cortex || pref === :auto
        use_lc = _try_load_liquid_cortex()
        if pref === :liquid_cortex && !use_lc
            error("LIMEN_RESERVOIR=liquid_cortex but LiquidCortex failed to load")
        end
    end

    if use_lc && (pref === :liquid_cortex || pref === :auto)
        println("[reservoir] backend=LiquidCortex n_in=$n_in n_out=$n_out")
        # LiquidCortex.EnsembleBrain is in Main after `using`
        ens = Main.LiquidCortex.EnsembleBrain(; n_in=n_in, n_out=n_out)
        return Reservoir(:liquid_cortex, ens, n_in, n_out)
    end

    # Local naut_core — must already be included by caller, or we include it.
    if !isdefined(Main, :EnsembleBrain) || !(Main.EnsembleBrain isa DataType)
        include(joinpath(@__DIR__, "naut_core.jl"))
    end
    println("[reservoir] backend=naut_core (local) n_in=$n_in n_out=$n_out")
    # naut_core uses compile-time N_IN; document mismatch if n_in != N_IN
    if isdefined(Main, :N_IN) && n_in != Main.N_IN
        @warn "naut_core N_IN=$(Main.N_IN) ≠ requested n_in=$n_in — using naut_core constants"
    end
    ens = Main.EnsembleBrain()
    return Reservoir(:naut_core, ens, n_in, n_out)
end

"""
    reservoir_step!(res, u; kwargs...) 

Step all lobes. Accept either a MarketPulse-like named kwargs or precomputed inhibition.

Keyword stress fields (Float32):
  gpu_temp, basys_load, funding_rate, liquidation_vol, liquidity_delta,
  dydx_oi_delta, dydx_funding_rate
"""
function reservoir_step!(res::Reservoir, u;
    gpu_temp::Float32=45.0f0,
    basys_load::Float32=0.1f0,
    funding_rate::Float32=0.0f0,
    liquidation_vol::Float32=0.0f0,
    liquidity_delta::Float32=0.0f0,
    dydx_oi_delta::Float32=0.0f0,
    dydx_funding_rate::Float32=0.0f0,
    inhibition::Union{Nothing,Float32}=nothing,
)
    inhib = if inhibition === nothing
        hft_inhibition(gpu_temp, basys_load, funding_rate, liquidation_vol;
            dydx_oi_delta=dydx_oi_delta, dydx_funding_rate=dydx_funding_rate)
    else
        inhibition
    end

    if res.backend === :liquid_cortex
        Main.LiquidCortex.ensemble_step!(res.ensemble, u;
            inhibition=inhib, reflex_signal=liquidity_delta)
    else
        # naut_core keyword API
        ensemble_step!(res.ensemble, u; inhibition=inhib, reflex_signal=liquidity_delta)
    end
    return nothing
end

"""Step using fields from a MarketPulse."""
function reservoir_step!(res::Reservoir, u, pulse)
    return reservoir_step!(res, u;
        gpu_temp=pulse.gpu_temp_c,
        basys_load=pulse.basys_buffer_load,
        funding_rate=pulse.funding_rate,
        liquidation_vol=pulse.liquidation_vol,
        liquidity_delta=pulse.liquidity_delta,
        dydx_oi_delta=pulse.dydx_oi_delta,
        dydx_funding_rate=pulse.dydx_funding_rate)
end

function reservoir_output(res::Reservoir)
    if res.backend === :liquid_cortex
        return Main.LiquidCortex.get_ensemble_output(res.ensemble)
    else
        return get_ensemble_output(res.ensemble)
    end
end

function reservoir_diagnostics(res::Reservoir)
    if res.backend === :liquid_cortex
        return Main.LiquidCortex.ensemble_diagnostics(res.ensemble)
    else
        return ensemble_diagnostics(res.ensemble)
    end
end

"""Access underlying ensemble (for NERO / spike recorder)."""
reservoir_ensemble(res::Reservoir) = res.ensemble

"""Lobe list for spike recording / NERO (both backends expose `.lobes`)."""
reservoir_lobes(res::Reservoir) = res.ensemble.lobes

function reservoir_status(res::Reservoir)
    return (backend=res.backend, n_in=res.n_in, n_out=res.n_out)
end
