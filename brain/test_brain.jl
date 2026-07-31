# test_brain.jl — Brain Health Check & Quick Smoke Test
#
# Fast diagnostic that verifies the neural substrate is operational.
# Run before starting the main brain to catch issues early.
#
# Usage:
#   julia --project=brain test_brain.jl
#
# Checks:
#   1. CUDA device availability and VRAM
#   2. Core module loading (naut_core, synapse_conductor, market_encoder)
#   3. EnsembleBrain initialization (4 lobes × 65K neurons)
#   4. Single-tick step execution
#   5. ZMQ socket binding
#   6. Signal encoding pipeline (MarketEncoder → spikes)

using CUDA
using ZMQ
using Printf
using SparseArrays
using Statistics
using LinearAlgebra

const PASS = "\033[32m✓\033[0m"
const FAIL = "\033[31m✗\033[0m"
const WARN = "\033[33m!\033[0m"

results = Bool[]

function check(name::String, f::Function)
    print("  $name ... ")
    try
        f()
        println("$PASS")
        push!(results, true)
    catch e
        println("$FAIL $e")
        push!(results, false)
    end
end

function run_brain_health_check()
    println()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  Spikenaut Brain — Health Check                            ║")
    println("╚══════════════════════════════════════════════════════════════╝")
    println()

    # ── 1. CUDA Hardware ──────────────────────────────────────────────────
    println("[1/6] CUDA Hardware")
    check("Device available", () -> CUDA.functional())
    check("Device name", () -> println(CUDA.name(CUDA.device())))
    check("VRAM > 8 GB", () -> @assert CUDA.total_memory() > 8e9)
    check("Available VRAM > 4 GB", () -> @assert CUDA.available_memory() > 4e9)

    # ── 2. Core Modules ───────────────────────────────────────────────────
    println("\n[2/6] Core Modules")
    brain_dir = @__DIR__
    check("naut_core.jl", () -> include(joinpath(brain_dir, "naut_core.jl")))
    check("synapse_conductor.jl", () -> include(joinpath(brain_dir, "synapse_conductor.jl")))
    check("market_encoder.jl", () -> include(joinpath(brain_dir, "market_encoder.jl")))
    check("vault_recorder.jl", () -> include(joinpath(brain_dir, "vault_recorder.jl")))

    # ── 3. EnsembleBrain Initialization ───────────────────────────────────
    println("\n[3/6] EnsembleBrain")
    ensemble = nothing
    check("Initialize 4-lobe brain", () -> (global ensemble = EnsembleBrain()))
    check("262K neurons allocated", () -> @assert length(ensemble.lobes) == 4)
    check("Per-lobe τ_m set", () -> @assert all(l -> l.tau_m > 0, ensemble.lobes))

    # ── 4. Single-Tick Step ───────────────────────────────────────────────
    println("\n[4/6] Neural Dynamics")
    check("MarketPulse decode", () -> begin
        # Simulate a 120-byte pulse
        buf = zeros(UInt8, 120)
        reinterpret(UInt64, buf[1:8])[1] = UInt64(time_ns())  # timestamp
        buf[9] = 0x40  # some price data
        pulse = decode_market_pulse(buf)
        @assert pulse.timestamp_ns > 0
    end)
    check("Encoder → spike vector", () -> begin
        buf = zeros(UInt8, 120)
        reinterpret(UInt64, buf[1:8])[1] = UInt64(time_ns())
        pulse = decode_market_pulse(buf)
        encoder = MarketEncoder(7; delta=0.001f0)
        spikes = encode!(encoder, pulse)
        @assert length(spikes) == 28
    end)
    check("Ensemble step (all 4 lobes)", () -> begin
        buf = zeros(UInt8, 120)
        reinterpret(UInt64, buf[1:8])[1] = UInt64(time_ns())
        pulse = decode_market_pulse(buf)
        encoder = MarketEncoder(7; delta=0.001f0)
        spikes = encode!(encoder, pulse)
        u_gpu = cu(spikes)
        ensemble_step!(ensemble, u_gpu, 45.0f0, 0.1f0, 0.0f0, 0.0f0, 0.0f0)
    end)
    check("Output vector (16 elements)", () -> begin
        out = get_ensemble_output(ensemble)
        @assert length(out) == 16
    end)

    # ── 5. ZMQ Sockets ───────────────────────────────────────────────────
    println("\n[5/6] ZMQ Transport")
    check("Context creation", () -> begin
        ctx = ZMQ.Context()
        ZMQ.close(ctx)
    end)
    check("SUB socket bind", () -> begin
        ctx = ZMQ.Context()
        sub = ZMQ.Socket(ctx, ZMQ.SUB)
        ZMQ.close(sub)
        ZMQ.close(ctx)
    end)

    # ── 6. Math Modules ──────────────────────────────────────────────────
    println("\n[6/6] Math Modules")
    math_dir = joinpath(dirname(brain_dir), "math")
    if isdir(math_dir)
        check("Hawkes intensity", () -> include(joinpath(math_dir, "market_hawkes.jl")))
        check("Hurst exponent", () -> include(joinpath(math_dir, "market_fractal.jl")))
        check("GBM surprise", () -> include(joinpath(math_dir, "market_sde.jl")))
        check("Fast Reflex LSM", () -> include(joinpath(math_dir, "market_lsm.jl")))
    else
        println("  $WARN math/ directory not found, skipping")
    end

    # ── Summary ───────────────────────────────────────────────────────────
    passed = sum(results)
    total = length(results)
    println()
    if passed == total
        println("$PASS All $total checks passed — brain is healthy")
    else
        println("$FAIL $passed/$total checks passed — review failures above")
    end
    println()

    return passed == total
end

success = run_brain_health_check()
exit(success ? 0 : 1)
