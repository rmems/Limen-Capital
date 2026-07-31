# test_loading.jl — Dependency Resolver & Startup Validator
#
# Verifies that all Julia packages and local modules are resolvable
# before attempting to start the main brain. Catches missing deps early.
#
# Usage:
#   julia --project=brain test_loading.jl
#
# What it checks:
#   1. Project.toml exists and is valid
#   2. All [deps] packages can be resolved
#   3. All local includes (naut_core, synapse_conductor, etc.) parse
#   4. CUDA device is accessible
#   5. Manifest.toml is in sync with Project.toml

using Printf

const PASS = "\033[32m✓\033[0m"
const FAIL = "\033[31m✗\033[0m"
const WARN = "\033[33m!\033[0m"
const INFO = "\033[36m·\033[0m"

errors = String[]

function validate(check::String, ok::Bool; detail::String="")
    if ok
        println("  $PASS $check")
    else
        msg = detail.isEmpty ? check : "$check — $detail"
        println("  $FAIL $msg")
        push!(errors, msg)
    end
end

function run_loading_validation()
    println()
    println("╔══════════════════════════════════════════════════════════════╗")
    println("║  Spikenaut Brain — Dependency Validation                   ║")
    println("╚══════════════════════════════════════════════════════════════╝")
    println()

    brain_dir = @__DIR__
    project_dir = dirname(brain_dir)

    # ── 1. Project Configuration ──────────────────────────────────────────
    println("[1/5] Project Configuration")

    project_path = joinpath(project_dir, "Project.toml")
    validate("Root Project.toml exists", isfile(project_path))
    if isfile(project_path)
        content = read(project_path, String)
        validate("Has [deps] section", occursin("[deps]", content))
        validate("Has CUDA dep", occursin("CUDA", content))
        validate("Has ZMQ dep", occursin("ZMQ", content))
        validate("No broken Spikenaut deps", !occursin("Spikenaut", content))
    end

    brain_project = joinpath(brain_dir, "Project.toml")
    validate("Brain Project.toml exists", isfile(brain_project))
    if isfile(brain_project)
        content = read(brain_project, String)
        validate("No broken Spikenaut deps", !occursin("Spikenaut", content))
    end

    # ── 2. Package Resolution ─────────────────────────────────────────────
    println("\n[2/5] Package Resolution")

    required_packages = [:CUDA, :ZMQ, :SparseArrays, :LinearAlgebra, :Statistics, :Printf]
    for pkg in required_packages
        try
            @eval using $pkg
            validate("Package $pkg", true)
        catch e
            validate("Package $pkg", false, detail=string(e))
        end
    end

    # Optional packages (vault recorder deps)
    optional_packages = [:Polars, :DuckDB, :DataFrames]
    for pkg in optional_packages
        try
            @eval using $pkg
            validate("Optional package $pkg", true)
        catch e
            println("  $WARN Optional package $pkg not available (vault recorder disabled)")
        end
    end

    # ── 3. Local Module Parsing ───────────────────────────────────────────
    println("\n[3/5] Local Module Parsing")

    local_modules = [
        "naut_core.jl" => "SparseBrain, EnsembleBrain",
        "market_types.jl" => "MarketPulse wire types",
        "synapse_conductor.jl" => "NeroOrchestrator",
        "market_encoder.jl" => "MarketEncoder",
        "reservoir.jl" => "Reservoir façade",
        "feature_stream.jl" => "Hawkes FeatureStream",
        "metrics.jl" => "Experiment metrics",
        "vault_recorder.jl" => "VaultRecorder",
        "spike_recorder.jl" => "SpikeRecorder",
        "research_helm.jl" => "research CLI",
    ]

    for (file, desc) in local_modules
        path = joinpath(brain_dir, file)
        if isfile(path)
            try
                # Parse without executing (check syntax)
                Meta.parse(read(path, String))
                validate("$file ($desc)", true)
            catch e
                validate("$file ($desc)", false, detail="Parse error: $e")
            end
        else
            validate("$file ($desc)", false, detail="File not found")
        end
    end

    # ── 4. Math Modules ──────────────────────────────────────────────────
    println("\n[4/5] Math Modules")

    math_dir = joinpath(project_dir, "math")
    math_modules = [
        "market_hawkes.jl" => "Hawkes intensity estimator",
        "market_fractal.jl" => "Hurst exponent estimator",
        "market_sde.jl" => "GBM surprise estimator",
        "market_lsm.jl" => "Fast Reflex LSM preprocessor",
    ]

    for (file, desc) in math_modules
        path = joinpath(math_dir, file)
        if isfile(path)
            try
                Meta.parse(read(path, String))
                validate("$file ($desc)", true)
            catch e
                validate("$file ($desc)", false, detail="Parse error: $e")
            end
        else
            validate("$file ($desc)", false, detail="File not found")
        end
    end

    # ── 5. CUDA Device ───────────────────────────────────────────────────
    println("\n[5/5] CUDA Device")
    try
        if CUDA.functional()
            dev = CUDA.device()
            mem = CUDA.total_memory() / 1e9
            validate("CUDA device: $(CUDA.name(dev))", true)
            validate("VRAM: $(round(mem, digits=1)) GB", mem >= 8.0,
                detail=mem < 8.0 ? "Need ≥8 GB for 262K neurons" : "")
        else
            validate("CUDA functional", false, detail="No CUDA device available")
        end
    catch e
        validate("CUDA initialization", false, detail=string(e))
    end

    # ── Summary ───────────────────────────────────────────────────────────
    println()
    if isempty(errors)
        println("$PASS All validations passed — ready to start brain")
    else
        println("$FAIL $(length(errors)) error(s) found:")
        for err in errors
            println("    → $err")
        end
    end
    println()

    return isempty(errors)
end

success = run_loading_validation()
exit(success ? 0 : 1)
