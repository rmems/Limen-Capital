# sonar_probe.jl — Hardware Benchmark & Latency Profiler
#
# Measures actual GPU compute latency for the core SNN operations.
# Use this to validate RTX 5080 performance before deploying the brain.
#
# Usage:
#   julia --project=brain sonar_probe.jl
#
# Benchmarks:
#   1. CUDA kernel launch overhead
#   2. Sparse mat-vec (W × S) — the hot path
#   3. Full step! latency (all 4 lobes)
#   4. Monte Carlo path generation throughput
#   5. End-to-end pipeline: encode → step → NERO → publish

using CUDA
using ZMQ
using Printf
using Statistics
using SparseArrays
using LinearAlgebra

const WARMUP_ITERS = 10
const BENCH_ITERS = 100

# ─── Helpers ──────────────────────────────────────────────────────────────────

function bench_ms(f; warmup=WARMUP_ITERS, iters=BENCH_ITERS)
    # Warmup
    for _ in 1:warmup
        f()
    end
    CUDA.synchronize()

    # Timed run
    times = zeros(Float64, iters)
    for i in 1:iters
        t0 = time_ns()
        f()
        CUDA.synchronize()
        times[i] = (time_ns() - t0) / 1e6  # ms
    end

    return (median=median(times), mean=mean(times), p95=quantile(times, 0.95), min=minimum(times))
end

function print_result(name, stats)
    @printf("  %-40s median=%6.2f ms  mean=%6.2f ms  p95=%6.2f ms  min=%6.2f ms\n",
        name, stats.median, stats.mean, stats.p95, stats.min)
end

# ─── Main Benchmark ───────────────────────────────────────────────────────────

function run_sonar_probe()
    println()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  Spikenaut Sonar Probe — Hardware Benchmark                ║")
    println("╚══════════════════════════════════════════════════════════════╝")
    println()

    # Device info
    dev = CUDA.device()
    total_mem = CUDA.total_memory() / 1e9
    free_mem = CUDA.available_memory() / 1e9
    @printf("  Device: %s\n", CUDA.name(dev))
    @printf("  VRAM: %.1f / %.1f GB (%.0f%% used)\n", total_mem - free_mem, total_mem, (1 - free_mem/total_mem) * 100)
    println()

    # ── 1. CUDA Kernel Launch Overhead ────────────────────────────────────
    println("[1/5] CUDA Kernel Launch Overhead")
    dummy = CUDA.zeros(Float32, 1024)
    stats = bench_ms(() -> CUDA.fill!(dummy, 1.0f0))
    print_result("fill! (1K floats)", stats)

    # ── 2. Sparse Mat-Vec (Hot Path) ─────────────────────────────────────
    println("\n[2/5] Sparse Matrix-Vector (W × S)")
    N = 65_536
    nnz = round(Int, N * N * 0.01)
    rows = rand(1:N, nnz)
    cols = rand(1:N, nnz)
    vals = Float16.(randn(Float32, nnz) .* 0.02f0)
    W = CUDA.CUSPARSE.CuSparseMatrixCSC(sparse(rows, cols, vals, N, N))
    S = CUDA.rand(Float32, N)

    stats = bench_ms(() -> W * Float16.(S))
    print_result("sparse W(65K×65K) × S(65K)", stats)

    # ── 3. Full Step Latency (Single Lobe) ───────────────────────────────
    println("\n[3/5] Full step! Latency (Single Lobe)")
    brain_dir = @__DIR__
    include(joinpath(brain_dir, "naut_core.jl"))

    lobe = SparseBrain(25.0f0; name="bench")
    u = cu(randn(Float32, 28) .* 0.1f0)

    stats = bench_ms(() -> step!(lobe, u, 45.0f0, 0.1f0))
    print_result("step! (65K neurons, τ=25ms)", stats)

    # ── 4. Ensemble Step (4 Lobes) ───────────────────────────────────────
    println("\n[4/5] Ensemble Step (4 Lobes × 65K)")
    include(joinpath(brain_dir, "synapse_conductor.jl"))
    ensemble = EnsembleBrain()

    stats = bench_ms(() -> ensemble_step!(ensemble, u, 45.0f0, 0.1f0, 0.0f0, 0.0f0, 0.0f0))
    print_result("ensemble_step! (262K total neurons)", stats)

    # ── 5. End-to-End Pipeline ───────────────────────────────────────────
    println("\n[5/5] End-to-End Pipeline")
    include(joinpath(brain_dir, "market_encoder.jl"))
    encoder = MarketEncoder(7; delta=0.0005f0)
    nero = NeroOrchestrator()

    # Simulate a MarketPulse
    buf = zeros(UInt8, 120)
    reinterpret(UInt64, buf[1:8])[1] = UInt64(time_ns())
    pulse = decode_market_pulse(buf)

    stats = bench_ms(() -> begin
        spikes = encode!(encoder, pulse)
        u_gpu = cu(spikes)
        ensemble_step!(ensemble, u_gpu, 45.0f0, 0.1f0, 0.0f0, 0.0f0, 0.0f0)
        update_relevance!(nero, ensemble)
        out = get_ensemble_output(ensemble)
    end)
    print_result("encode → step → NERO → output", stats)

    # ── Summary ───────────────────────────────────────────────────────────
    println()
    println("  Benchmark complete. Target for HFT: <1ms per tick.")
    println()

    # ── VRAM After Benchmark ──────────────────────────────────────────────
    free_after = CUDA.available_memory() / 1e9
    used_after = total_mem - free_after
    @printf("  VRAM after bench: %.1f / %.1f GB (%.0f%% used)\n", used_after, total_mem, used_after / total_mem * 100)
    println()
end

run_sonar_probe()
