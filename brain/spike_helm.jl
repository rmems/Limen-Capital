# spike_helm.jl — Spikenaut Helm Control System
#
# The command center and navigation helm for the Spikenaut neuromorphic system.
# Acts as the biological "brain" that receives sensory input and issues commands.
# Subscribes to the Rust Nervous System via secure ZMQ TCP communication.
#
# Architecture:
#   4 parallel neural lobes (Scalper, Day, Swing, Macro) × 65,536 LIF neurons each
#   Varying biological time constants: τ_m ∈ {10ms, 25ms, 50ms, 100ms}
#   Xavier/Glorot W_out initialization (breaks zero-readout deadlock)
#   Sparse connectivity (1% connection probability, Float16 weights)
#   STDP covariance learning with biological reinforcement
#   Rolling 1,000-tick spike history for deep temporal memory
#   Global inhibition from hardware proprioception (pain receptors)
#
# Hardware Target: RTX 5080 (16GB VRAM) - Biological neural substrate
# Expected VRAM usage:
#   4 × W_sparse: 65536² × 1% × ~6 bytes/nnz   ≈ 1.0 GB
#   4 × W_in:     65536 × 14 × 4 bytes           ≈ 15 MB
#   4 × W_out:    16 × 65536 × 4 bytes            ≈ 17 MB
#   4 × State vectors (V, S, traces):             ≈ 21 MB
#   4 × History (1000 × 65536 × 4 bytes):         ≈ 1.05 GB
#   MC buffers (65536 paths × 200 × 7):           ≈ 3.67 GB
#   Covariance working memory (8192²):            ≈ 0.5 GB
#   Total peak: ~12–14 GB VRAM
#
# Usage:
#   julia --project=spikenaut-capital/brain spike_helm.jl
#
# ═══════════════════════════════════════════════════════════════════════════════

using ZMQ
using CUDA
using Printf

# Load the neural core modules
include(joinpath(@__DIR__, "naut_core.jl"))
include(joinpath(@__DIR__, "reservoir.jl"))
include(joinpath(@__DIR__, "synapse_conductor.jl"))
include(joinpath(@__DIR__, "market_encoder.jl"))
include(joinpath(@__DIR__, "feature_stream.jl"))
include(joinpath(@__DIR__, "vault_recorder.jl"))

# ─── Constants ────────────────────────────────────────────────────────────────

# ZMQ configuration (matches Rust side — research defaults are plain TCP)
const IPC_ENDPOINT = get(ENV, "LIMEN_IPC_SUB", "tcp://127.0.0.1:5555")   # Receive from Rust
const IPC_PUB_ENDPOINT = get(ENV, "LIMEN_IPC_PUB", "tcp://127.0.0.1:5556") # Send to Rust
include(joinpath(@__DIR__, "zmq_curve.jl"))
using .ZmqCurve
const COVARIANCE_INTERVAL = 50   # Compute subsampled covariance every N ticks
# MC off-by-default for experimental iteration; set LIMEN_MC_PATHS to enable
const MC_PATHS = parse(Int, get(ENV, "LIMEN_MC_PATHS", "0"))
const MC_HORIZON = 200           # MC simulation horizon (ticks)
const MC_INTERVAL = 5            # Run MC simulation every N ticks

# ─── Resource Management ────────────────────────────────────────────────────────

function check_cuda_memory()
    if !CUDA.functional()
        error("CUDA not available - cannot run brain")
    end

    available = CUDA.available_memory()
    if MC_PATHS <= 0
        println("[brain] Memory check: MC disabled (LIMEN_MC_PATHS=0); available $(round(available/1e9, digits=2)) GB")
        return true
    end

    required = MC_PATHS * MC_HORIZON * 7 * 4  # 4 bytes per float32
    println("[brain] Memory check: MC requires $(round(required/1e9, digits=2))GB, available $(round(available/1e9, digits=2))GB")

    if required > available * 0.9  # Leave 10% buffer
        error("Insufficient GPU memory: need $(round(required/1e9, digits=2))GB, have $(round(available/1e9, digits=2))GB")
    end

    return true
end

# ─── ZMQ Setup ────────────────────────────────────────────────────────────────

function setup_zmq()
    validate_binary_endpoint(IPC_ENDPOINT)
    validate_binary_endpoint(IPC_PUB_ENDPOINT)
    ctx = ZMQ.Context()
    zap_state = nothing

    allowlist = ZmqCurve.curve_enabled() ? ZmqCurve.load_curve_allowlist() : nothing
    if allowlist !== nothing
        zap_state = ZmqCurve.start_zap_allowlist!(ctx, allowlist)
        println("[brain] ZAP allowlist active ($(length(allowlist)) client keys)")
    end

    # SUB socket: receive MarketPulse from Rust (CURVE client)
    sub = ZMQ.Socket(ctx, ZMQ.SUB)
    if ZmqCurve.curve_enabled()
        sec = ZmqCurve.client_secret()
        sec === nothing && error("ZMQ_CURVE=1 requires ZMQ_CLIENT_SECRET_KEY or ZMQ_SERVER_KEY")
        mpk = ZmqCurve.market_server_public()
        mpk === nothing && error("ZMQ_CURVE=1 requires ZMQ_MARKET_SERVER_PUBLIC_KEY for market SUB")
        ZmqCurve.apply_curve_client!(sub, mpk, sec)
        println("[brain] CURVE client on SUB (market feed)")
    end
    ZMQ.subscribe(sub, "")
    ZMQ.connect(sub, IPC_ENDPOINT)
    println("[brain] ZMQ SUB connected to $IPC_ENDPOINT")

    # PUB socket: send readout signals back to Rust (CURVE server)
    pub = ZMQ.Socket(ctx, ZMQ.PUB)
    if ZmqCurve.curve_enabled()
        sec = ZmqCurve.readout_server_secret()
        sec === nothing && error("ZMQ_CURVE=1 requires ZMQ_READOUT_SERVER_SECRET or ZMQ_SERVER_KEY for PUB")
        ZmqCurve.apply_curve_server!(pub, sec; zap=allowlist !== nothing)
        println("[brain] CURVE server on PUB (readout)")
    end
    ZMQ.bind(pub, IPC_PUB_ENDPOINT)
    println("[brain] ZMQ PUB bound to $IPC_PUB_ENDPOINT")

    return ctx, sub, pub, zap_state
end

# ─── Readout Publishing ──────────────────────────────────────────────────────

"""
    publish_readout!(buf::Vector{UInt8}, pub::ZMQ.Socket,
                    output::Vector{Float32}, relevance::Vector{Float32}, tick::Int64)

Publish the 16-element readout vector + 4 NERO relevance scores via ZMQ.

Format: NW1R + version/reserved header (8 bytes), then 88-byte LE payload.
The tick is the originating MarketPulse Unix timestamp in nanoseconds.
Non-finite output/relevance is rejected before sending.
"""
function publish_readout!(buf::Vector{UInt8}, pub::ZMQ.Socket,
                          output::Vector{Float32}, relevance::Vector{Float32},
                          tick::Int64)
    # NervousWire v1: reuse pack_readout (market_types.jl via naut_core)
    packed = frame_payload("NW1R", pack_readout(tick, output, relevance))
    resize!(buf, length(packed))
    copyto!(buf, packed)
    ZMQ.send(pub, buf)
end

# ═══════════════════════════════════════════════════════════════════════════════
# Main Brain Loop
# ═══════════════════════════════════════════════════════════════════════════════

function main()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  Spikenaut V3 Enhanced Brain — 4-Lobe Ensemble × 65,536 Neurons ║")
    println("║  RTX 5080 (16GB) │ OU-SDE + STDP │ Secure ZMQ IPC           ║")
    println("║  MC: 1500000 paths × 200 horizon │ Fan-Boost ~13 GB          ║")
    println("║  Mining-chain SNN │ 120-byte wire │ DNX/Quai/Qubic/KAS/XMR ║")
    println("╚══════════════════════════════════════════════════════════════╝")

    # ── Security & Resource Checks ─────────────────────────────────────────────────
    println("[brain] Performing security and resource checks...")
    check_cuda_memory()
    println("[brain] ✓ CUDA memory check passed")

    # ── CUDA warm-up ─────────────────────────────────────────────────────────
    println("[brain] CUDA device: ", CUDA.name(CUDA.device()))
    println("[brain] CUDA memory: ", round(CUDA.available_memory() / 1e9, digits=2), " GB available")

    # ── Reservoir façade (LiquidCortex if available, else naut_core) ─────────
    res = build_reservoir(; n_in=28, n_out=16)
    ensemble = reservoir_ensemble(res)  # NERO / vault still expect ensemble
    println("[brain] reservoir status: ", reservoir_status(res))

    # ── Initialize NERO orchestrator ─────────────────────────────────────────
    nero = NeroOrchestrator()

    # ── ZMQ connections ──────────────────────────────────────────────────────
    ctx, sub, pub, zap_state = setup_zmq()

    # ── Initialize Market Encoder & Vault Recorder (Augmentation) ────────────
    println("[brain] Activating Neuromorphic Augmentation...")
    encoder = MarketEncoder(7; delta=0.0005f0)
    features = FeatureStream(; base_delta=0.0005f0)
    println("[brain] FeatureStream: Hawkes self-excitation → encoder δ + Scalper reflex")
    vault = VaultRecorder(100) # Commit to DuckDB every 100 ticks

    # ── Pre-allocate 88-byte readout buffer (zero allocation in hot loop) ────
    readout_buf = Vector{UInt8}(undef, 88)

    # ── Volatility estimates for Monte Carlo (initialized to default) ────────
    # Volatility estimates per mining chain (updated from covariance every 50 ticks)
    # DNX=high, Quai=high, Qubic=very high, Kaspa=high, Monero=low (mature), Ocean=med, Verus=high
    vol_estimates = Float32[0.05, 0.06, 0.08, 0.06, 0.02, 0.04, 0.07]  # DNX,Quai,Qubic,Kaspa,Monero,Ocean,Verus

    # ── Optional MC buffer (disabled when LIMEN_MC_PATHS=0) ─────────────────
    mc_result_buf = if MC_PATHS > 0
        CUDA.zeros(Float32, MC_PATHS, MC_HORIZON, 7)
    else
        nothing
    end

    CUDA.synchronize()
    free_mem = CUDA.available_memory() / 1e9
    total_mem = CUDA.total_memory() / 1e9
    used = total_mem - free_mem
    @printf("[brain] Post-init VRAM: %.2f / %.2f GB (%.0f%% used)\n", used, total_mem, used / total_mem * 100)
    println("[brain] Entering main loop — waiting for Rust Nervous System...")
    println()

    tick = 0
    mc_tick = 0
    last_pulse_ns = UInt64(round(Int64, time() * 1e9))

    try
        while true
            # ── 1. Receive MarketPulse from Rust (blocking) ──────────────────
            msg = ZMQ.recv(sub)
            raw = Vector{UInt8}(msg)

            # ── Input Validation ───────────────────────────────────────────────
            # ── 2. Decode first (fail-closed); only then advance experiment tick ─
            pulse = try
                decode_market_pulse(unframe_payload(raw, "NW1P", 120))
            catch e
                @warn "Rejecting invalid MarketPulse frame" exception = e
                continue
            end
            now = UInt64(round(Int64, time() * 1e9))
            if !(last_pulse_ns < pulse.timestamp_ns <= now)
                @warn "Rejecting non-increasing, pre-start, or future market timestamp"
                continue
            end
            last_pulse_ns = pulse.timestamp_ns
            tick += 1

            # ── 2.5 FeatureStream (Hawkes on DNX price) ───────────────────────
            snap = update_features!(features, Float64(pulse.dnx_price), Float64(tick))
            apply_encoder_gain!(encoder, snap)
            # Inject Hawkes excess into liquidity_delta slot for Scalper flash-learning
            # (MarketPulse is immutable struct — pass via reservoir_step kwargs)
            # ── 3. Build spiking input vector and transfer to GPU ────────────────
            u_cpu = encode!(encoder, pulse)
            u_gpu = cu(u_cpu)

            # ── 4. Reservoir step (unified inhibition + Hawkes reflex_signal) ──
            reservoir_step!(res, u_gpu;
                gpu_temp=pulse.gpu_temp_c,
                basys_load=pulse.basys_buffer_load,
                funding_rate=pulse.funding_rate,
                liquidation_vol=pulse.liquidation_vol,
                liquidity_delta=max(pulse.liquidity_delta, snap.reflex_signal),
                dydx_oi_delta=pulse.dydx_oi_delta,
                dydx_funding_rate=pulse.dydx_funding_rate)

            # ── 4.5. Record to Vault (DuckDB) ───────────────────────
            record_tick!(vault, pulse, ensemble)

            # ── 5. Periodic covariance on each lobe (GPU-intensive) ──────────
            # Uses subsampled 8192 neurons per lobe → 268 MB covariance matrix
            if tick % COVARIANCE_INTERVAL == 0
                for (li, lobe) in enumerate(ensemble.lobes)
                    result = compute_reservoir_covariance!(lobe)
                    if result !== nothing
                        C, indices = result
                        # Extract diagonal variance from subsampled covariance
                        diag_var = Array(CUDA.diag(C))
                        # Map subsampled neuron groups to asset volatility proxies
                        sub_group = div(COV_SUBSAMPLE, 7)
                        for a in 1:7
                            start_idx = (a - 1) * sub_group + 1
                            end_idx = min(a * sub_group, length(diag_var))
                            if end_idx >= start_idx
                                v = clamp(sqrt(mean(diag_var[start_idx:end_idx])), 0.001f0, 0.5f0)
                                # Blend across lobes: newer estimates weighted more
                                vol_estimates[a] = 0.7f0 * vol_estimates[a] + 0.3f0 * v
                            end
                        end
                    end
                end
            end

            # ── 6. Optional Monte Carlo (only if LIMEN_MC_PATHS > 0) ──────────
            if mc_result_buf !== nothing && tick % MC_INTERVAL == 0
                mc_tick += 1
                current_prices = Float32[
                    pulse.dnx_price, pulse.quai_price, pulse.qubic_price,
                    pulse.kaspa_price, pulse.monero_price,
                    pulse.ocean_price, pulse.verus_price
                ]
                if any(abs.(current_prices) .> 0.001f0)
                    monte_carlo_paths!(mc_result_buf, current_prices, vol_estimates)
                end
            end

            # ── 7–8. NERO relevance (causal) → re-aggregate → publish ────────
            # route_and_aggregate!: relevance weights replace static LOBE_WEIGHTS
            output = route_and_aggregate!(nero, ensemble)
            if output === nothing
                @warn "Non-finite neural state; reset NERO and skipped readout"
                continue
            end
            publish_readout!(readout_buf, pub, output, nero.relevance, Int64(pulse.timestamp_ns))

            # ── 9. Diagnostics ───────────────────────────────────────────────
            if tick % 20 == 0
                println(reservoir_diagnostics(res))
                println(nero_diagnostics(nero))

                # GPU memory status
                free_mem = CUDA.available_memory() / 1e9
                total_mem = CUDA.total_memory() / 1e9
                used_pct = (1.0 - free_mem / total_mem) * 100
                @printf("[brain] VRAM: %.1f/%.1f GB (%.0f%% used) | MC total: %d paths\n",
                    total_mem - free_mem, total_mem, used_pct, mc_tick * MC_PATHS)
            end
        end

    catch e
        if isa(e, InterruptException)
            println("\n[brain] Ctrl+C — shutting down gracefully")
        else
            println("[brain] ERROR: ", e)
            println(stacktrace(catch_backtrace()))
        end
    finally
        # Cleanup
        if @isdefined vault
            close_vault!(vault)
        end
        if @isdefined(zap_state) && zap_state !== nothing
            ZmqCurve.stop_zap!(zap_state)
        end
        ZMQ.close(sub)
        ZMQ.close(pub)
        ZMQ.close(ctx)
        println("[brain] ZMQ sockets closed. Total ticks: $tick")
    end
end

# ── Entry Point ──────────────────────────────────────────────────────────────
main()
