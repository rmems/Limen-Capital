module LimenCapital

using Random, SHA, TOML, Statistics, LinearAlgebra, JSON3
import LiquidCortex
using DendriteTrader
using DendriteTrader.Models
using DendriteTrader.Market
using DendriteTrader.Simulation
import DendriteTrader.Models: fit!, predict!, reset_state!

include("../brain/reservoir.jl")
include("forecast_adapter.jl")
include("comparison.jl")

export ReservoirForecast, run_comparison, qualify_backend, checkpoint_id

end
