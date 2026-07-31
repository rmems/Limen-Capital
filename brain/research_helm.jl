# research_helm.jl — Thin CLI over ExperimentRunner
#
# Usage:
#   julia --project=. research_helm.jl --duration 200 --seed 42
#   julia --project=. research_helm.jl --help
#
# Artifacts: runs/<run_id>/{meta.json,metrics.json[,spikes.bin]}

using CUDA
using Printf
using Statistics
using Random

include(joinpath(@__DIR__, "naut_core.jl"))
include(joinpath(@__DIR__, "reservoir.jl"))
include(joinpath(@__DIR__, "market_encoder.jl"))
include(joinpath(@__DIR__, "synapse_conductor.jl"))
include(joinpath(@__DIR__, "experiment_runner.jl"))

cfg = parse_research_args(ARGS)
# Shorter default for interactive runs if no args
if isempty(ARGS)
    cfg.duration = 200
    println("[research] no args — using duration=$(cfg.duration) seed=$(cfg.seed) (pass --help)")
end

run_experiment(cfg)
