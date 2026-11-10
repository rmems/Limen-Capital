# Fixed-spike CPU replay of the local readout/NERO rules. No GPU or market feed.
# Legacy equations are frozen from 93cc0f26684e7df5ab2744097af1fcb7bada1494:
# naut_core.jl (RMS scaling, potentiation-only readout) and synapse_conductor.jl
# (momentum zeroed by copying relevance before scoring).
module ModelMathComparison
using Random, LinearAlgebra, SparseArrays, Printf
include(joinpath(@__DIR__, "model_math.jl"))
const N_LOBES = 4
const N_OUT = 16
include(joinpath(@__DIR__, "synapse_conductor.jl"))

function legacy_readout!(W, pre, spikes, output, tick)
    pre .= pre .* 0.995f0 .+ spikes
    if tick % 10 == 0
        W .+= 0.001f0 .* (Float32.(output .> 0.0f0) * pre')
        clamp!(W, -1.0f0, 1.0f0)
    end
    output .= W * spikes
end

function comparison(; seed=2443, neurons=256, ticks=2000)
    rng = MersenneTwister(seed)
    # Match local initialization's COO collisions and Float16 quantization.
    nnz_expected = round(Int, neurons^2 * 0.08)
    recurrent = sparse(rand(rng, 1:neurons, nnz_expected), rand(rng, 1:neurons, nnz_expected),
        Float16.(0.02f0 .* randn(rng, Float32, nnz_expected)), neurons, neurons)
    for i in 1:neurons
        recurrent[i, i] = 0
    end
    legacy_proxy = norm(recurrent) / sqrt(nnz(recurrent))
    corrected_proxy = spectral_radius_proxy(recurrent)
    old_rec = recurrent .* Float16(0.9f0 / legacy_proxy)
    new_rec = recurrent .* Float16(0.9f0 / corrected_proxy)
    old_rho = maximum(abs, eigvals(Matrix{Float64}(old_rec)))
    new_rho = maximum(abs, eigvals(Matrix{Float64}(new_rec)))

    initial = [randn(rng, Float32, N_OUT, neurons) .* sqrt(2.0f0 / neurons) for _ in 1:4]
    old_W, new_W = deepcopy(initial), deepcopy(initial)
    old_pre = [zeros(Float32, neurons) for _ in 1:4]
    new_pre = deepcopy(old_pre)
    new_post = [zeros(Float32, N_OUT) for _ in 1:4]
    old_outputs = [zeros(Float32, N_OUT) for _ in 1:4]
    new_outputs = deepcopy(old_outputs)
    old_nero, new_nero = NeroOrchestrator(), NeroOrchestrator()
    old_agg, new_agg = zeros(Float32, N_OUT), zeros(Float32, N_OUT)
    old_abs = new_abs = difference = 0.0
    old_decreases = new_decreases = 0
    old_nero_momentum = new_nero_momentum = 0.0
    for tick in 1:ticks
        rates = zeros(Float32, 4)
        for lobe in 1:4
            # Same externally fixed spike tape for both rules; no endogenous
            # reservoir feedback. Vary engagement so NERO sees regime changes.
            probability = 0.02 + 0.12 * (0.5 + 0.5 * sin(tick / 50 + lobe))
            spikes = Float32.(rand(rng, Float32, neurons) .< probability)
            rates[lobe] = sum(spikes) / neurons
            previous_old, previous_new = copy(old_W[lobe]), copy(new_W[lobe])
            legacy_readout!(old_W[lobe], old_pre[lobe], spikes, old_outputs[lobe], tick)
            update_readout!(new_W[lobe], new_pre[lobe], new_post[lobe], spikes, new_outputs[lobe];
                decay=0.995f0, eta=0.001f0, w_max=1.0f0, learn=tick % 10 == 0)
            old_decreases += count(old_W[lobe] .< previous_old)
            new_decreases += count(new_W[lobe] .< previous_new)
        end
        old_ensemble = (lobes=[(output=old_outputs[i], last_spike_rate=rates[i]) for i in 1:4],
                        agg_output=old_agg, weights=fill(0.25f0, 4))
        new_ensemble = (lobes=[(output=new_outputs[i], last_spike_rate=rates[i]) for i in 1:4],
                        agg_output=new_agg, weights=fill(0.25f0, 4))
        # Exact legacy momentum semantics without copying the entire old NERO.
        copyto!(old_nero.prev_relevance, old_nero.relevance)
        old_nero_momentum += sum(abs.(old_nero.relevance .- old_nero.prev_relevance))
        new_nero_momentum += sum(abs.(new_nero.relevance .- new_nero.prev_relevance))
        route_and_aggregate!(old_nero, old_ensemble)
        route_and_aggregate!(new_nero, new_ensemble)
        old_abs += sum(abs, old_agg)
        new_abs += sum(abs, new_agg)
        difference += sum(abs, old_agg .- new_agg)
    end
    saturation(weights) = sum(count(abs.(W) .>= 0.999f0) for W in weights) / (4 * N_OUT * neurons)
    return (; seed, neurons, ticks, old_rho, new_rho,
        old_abs=old_abs / (ticks * N_OUT), new_abs=new_abs / (ticks * N_OUT),
        mean_abs_difference=difference / (ticks * N_OUT),
        old_saturation=saturation(old_W), new_saturation=saturation(new_W),
        old_decreases, new_decreases,
        old_momentum=old_nero_momentum / ticks, new_momentum=new_nero_momentum / ticks,
        old_final=copy(old_agg), new_final=copy(new_agg))
end

function main()
    BLAS.set_num_threads(1)
    result = withenv("LIMEN_AGG_MODE" => "relevance") do
        comparison()
    end
    @printf("Fixed-spike CPU comparison: seed=%d neurons=%d lobes=4 outputs=16 ticks=%d\n",
        result.seed, result.neurons, result.ticks)
    println("metric | before | after")
    @printf("measured recurrent spectral radius | %.6f | %.6f\n", result.old_rho, result.new_rho)
    @printf("mean absolute aggregated readout | %.6f | %.6f\n", result.old_abs, result.new_abs)
    @printf("saturated readout weights (fraction) | %.6f | %.6f\n", result.old_saturation, result.new_saturation)
    @printf("weight-decrease events | %d | %d\n", result.old_decreases, result.new_decreases)
    @printf("mean relevance momentum (L1/tick) | %.6f | %.6f\n", result.old_momentum, result.new_momentum)
    @printf("mean absolute before/after readout difference: %.6f\n", result.mean_abs_difference)
    println("final before readout: ", result.old_final)
    println("final after readout:  ", result.new_final)
    return result
end
end

if abspath(PROGRAM_FILE) == @__FILE__
    ModelMathComparison.main()
end
