# simple_test.jl — Quick System Status Overview
#
# One-shot status check that prints a compact summary of Limen-Capital.
# No package loading required — pure file checks.
#
# Usage:
#   julia brain/simple_test.jl

using Printf

brain_dir = @__DIR__
project_dir = dirname(brain_dir)

println()
println("╔══════════════════════════════════════════════════════════════╗")
println("║  Limen-Capital — System Status                             ║")
println("╚══════════════════════════════════════════════════════════════╝")
println()

# ── Brain Core ────────────────────────────────────────────────────────────────
println("[ Brain Core ]")
core_files = [
    "naut_core.jl"          => "65K-neuron sparse brain",
    "synapse_conductor.jl"  => "NERO orchestrator",
    "market_encoder.jl"     => "Market→spike encoder",
    "spike_helm.jl"         => "Main brain entry point",
    "vault_recorder.jl"     => "DuckDB storage",
    "spike_recorder.jl"     => "Binary spike logger",
    "research_helm.jl"      => "Research simulation",
]

for (file, desc) in core_files
    path = joinpath(brain_dir, file)
    if isfile(path)
        size_kb = filesize(path) / 1024
        @printf("  ✓ %-25s %5.1f KB  %s\n", file, size_kb, desc)
    else
        @printf("  ✗ %-25s  MISSING  %s\n", file, desc)
    end
end

# ── Math Modules ──────────────────────────────────────────────────────────────
println("\n[ Math Modules ]")
math_files = [
    "market_hawkes.jl"  => "Hawkes self-excitation",
    "market_fractal.jl" => "Hurst exponent",
    "market_sde.jl"     => "GBM surprise z-score",
    "market_lsm.jl"     => "Fast Reflex preprocessor",
]

for (file, desc) in math_files
    path = joinpath(project_dir, "math", file)
    if isfile(path)
        size_kb = filesize(path) / 1024
        @printf("  ✓ %-25s %5.1f KB  %s\n", file, size_kb, desc)
    else
        @printf("  ✗ %-25s  MISSING  %s\n", file, desc)
    end
end

# ── Execution Engine ──────────────────────────────────────────────────────────
println("\n[ Execution Engine ]")
exec_files = [
    "src/main.rs"    => "ZMQ listener daemon",
    "src/lib.rs"     => "Execution engine core",
    "src/kelly.rs"   => "Kelly criterion sizing",
    "src/wire.rs"    => "NervousWire pack/decode",
    "src/dydx.rs"    => "dydx market feed",
    "Cargo.toml"     => "Rust dependencies",
]

for (file, desc) in exec_files
    path = joinpath(project_dir, "execution", file)
    if isfile(path)
        size_kb = filesize(path) / 1024
        @printf("  ✓ %-25s %5.1f KB  %s\n", file, size_kb, desc)
    else
        @printf("  ✗ %-25s  MISSING  %s\n", file, desc)
    end
end

# ── Configuration ─────────────────────────────────────────────────────────────
println("\n[ Configuration ]")
config_files = [
    joinpath(project_dir, "Project.toml")          => "Julia deps",
    joinpath(brain_dir, "Project.toml")             => "Brain deps",
    joinpath(project_dir, "execution/Cargo.toml")   => "Rust deps",
    joinpath(project_dir, ".gitignore")             => "Git ignore",
]

for (path, desc) in config_files
    if isfile(path)
        @printf("  ✓ %-40s  %s\n", basename(path), desc)
    else
        @printf("  ✗ %-40s  MISSING  %s\n", basename(path), desc)
    end
end

# ── Environment ───────────────────────────────────────────────────────────────
println("\n[ Environment ]")

julia_ver = string(VERSION)
@printf("  Julia %s\n", julia_ver)

cuda_avail = try
    run(`nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` |> stdout)
    true
catch
    false
end

if cuda_avail
    @printf("  ✓ CUDA available\n")
else
    @printf("  ⚠ CUDA not detected (brain needs GPU)\n")
end

# ── Summary ───────────────────────────────────────────────────────────────────
total_core = length(core_files)
total_math = length(math_files)
total_exec = length(exec_files)

existing_core = count(f -> isfile(joinpath(brain_dir, f[1])), core_files)
existing_math = count(f -> isfile(joinpath(project_dir, "math", f[1])), math_files)
existing_exec = count(f -> isfile(joinpath(project_dir, "execution", f[1])), exec_files)

println()
@printf("  Status: %d/%d brain | %d/%d math | %d/%d execution\n",
    existing_core, total_core, existing_math, total_math, existing_exec, total_exec)

if existing_core == total_core && existing_math == total_math
    println("  ✓ System ready")
else
    println("  ⚠ Missing components — run test_brain.jl for details")
end
println()
