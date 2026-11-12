# CPU/GPU-generic model math, kept independent of CUDA for regression tests.
using LinearAlgebra
using Random

"""
    spectral_radius_proxy(W)

Random, zero-mean recurrent matrix estimate ||W||_F / sqrt(N), NOT an
eigenvalue bound or a guarantee of the echo-state property. Accumulate in
Float64 so Float16 weights cannot overflow the norm reduction.
"""
function spectral_radius_proxy(W::AbstractMatrix)
    size(W, 1) == size(W, 2) || throw(ArgumentError("recurrent matrix must be square"))
    size(W, 1) > 0 || throw(ArgumentError("recurrent matrix must be nonempty"))
    return sqrt(sum(x -> abs2(Float64(x)), W) / size(W, 1))
end

"""
    update_readout!(W, trace_pre, trace_post, spikes, output; decay, eta, w_max, learn)

Balanced pair STDP on the readout. A current positive readout potentiates
recent presynaptic activity; current presynaptic activity depresses weights
of recently positive readouts. Traces contain only past events when learning
is applied, so simultaneous events do not create an artificial positive bias.
`trace_post` has one entry per OUTPUT, not per reservoir neuron.
"""
function update_readout!(W, trace_pre, trace_post, spikes, output;
    decay, eta, w_max, learn::Bool)
    output .= W * spikes
    post = Float32.(output .> 0.0f0)
    trace_pre .*= decay
    trace_post .*= decay
    if learn
        W .+= eta .* (post * trace_pre' .- trace_post * spikes')
        clamp!(W, -w_max, w_max)
        output .= W * spikes
    end
    trace_pre .+= spikes
    trace_post .+= post
    return nothing
end

"""Seed the live task RNG before constructing the reservoir or encoder."""
function seed_live_rng!(seed_text=get(ENV, "LIMEN_SEED", "42"); io::IO=stdout)
    seed = tryparse(UInt64, strip(seed_text))
    seed === nothing && throw(ArgumentError("LIMEN_SEED must be an unsigned 64-bit integer"))
    Random.seed!(seed)
    println(io, "[brain] LIMEN_SEED=$seed (host RNG; seed CUDA before reservoir startup)")
    return seed
end

"""Preserve the sign of the strongest shock; negative liquidity must flash-learn too."""
strongest_reflex(liquidity_delta, hawkes_signal) =
    abs(liquidity_delta) >= abs(hawkes_signal) ? liquidity_delta : hawkes_signal

covariance_due(mc_paths, tick, interval) = mc_paths > 0 && tick % interval == 0
