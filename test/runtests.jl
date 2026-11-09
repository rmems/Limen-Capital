using Test, LimenCapital, DendriteTrader, JSON3, Random
const LC = LimenCapital

function tiny_frame(start=1, n=24)
    FeatureFrame([FeatureRow(i*1000, i, 2, 100+i%3, 100.1+i%3,
        (-1)^i * 0.5, i%4-2) for i in start:start+n-1])
end
function tiny_targets()
    [MovementTarget(i,i+4,4, (Down,Flat,Up)[mod1(i,3)]) for i in 1:20]
end

@testset "CPU backend qualification and isolation" begin
    @test qualify_backend().finite_active_output
    a, b = ReservoirForecast(), ReservoirForecast()
    @test a.reservoir.ensemble.W == b.reservoir.ensemble.W
    @test a.reservoir.ensemble.x !== b.reservoir.ensemble.x
    @test LC.reservoir_diagnostics(a.reservoir).neurons == 32
    @test LC.reservoir_status(a.reservoir).n_in == 5
    @test_throws ArgumentError LC.reservoir_lobes(a.reservoir)
    # An unrelated live-helm override cannot select a GPU backend.
    withenv("LIMEN_RESERVOIR"=>"naut_core") do
        @test ReservoirForecast().reservoir.backend == :reference_cpu
    end
    @test_throws ArgumentError ReservoirForecast(;encoding=:bogus)
    @test_throws DimensionMismatch LC.reservoir_step!(a.reservoir,ones(Float32,4);inhibition=0.0f0)
end

@testset "Pinned forecast contract, causal fit and deterministic boundaries" begin
    frame, targets = tiny_frame(), tiny_targets()
    model = ReservoirForecast()
    h = ForecastHorizon(4)
    @test_throws ArgumentError predict!(model,frame,h)
    fit!(model,frame,targets)
    id = checkpoint_id(model)
    scales, weights = copy(model.scales), copy(model.readout.weights)
    later = tiny_frame(40)
    batch = predict!(model,later,h)
    @test all(f -> f.classes == (Down,Flat,Up) && sum(f.probabilities) ≈ 1, batch)
    @test all(f -> all(isfinite,f.probabilities),batch)
    @test [f.sequence for f in batch] == [r.sequence for r in later]
    @test all(f -> f.horizon == h,batch)
    @test_throws ArgumentError predict!(model,later,h)
    reset_state!(model)
    chunked = [only(predict!(model,FeatureFrame([r]),h)) for r in later]
    @test [f.probabilities for f in batch] == [f.probabilities for f in chunked]
    @test checkpoint_id(model) == id
    # Extreme held-out values cannot change preprocessing or checkpoint weights.
    reset_state!(model)
    extreme = FeatureFrame([FeatureRow(100_000,100,2,1e6,1e6,1,1e6)])
    predict!(model,extreme,h)
    @test model.scales == scales
    @test model.readout.weights == weights
    @test checkpoint_id(model) == id
    @test_throws ArgumentError predict!(model,tiny_frame(200),ForecastHorizon(3))
    crossing = vcat(targets,[MovementTarget(24,28,4,Up)])
    @test_throws ArgumentError fit!(ReservoirForecast(),frame,crossing)
    mixed = copy(targets)
    mixed[1] = MovementTarget(1,5,3,Up)
    @test_throws ArgumentError fit!(ReservoirForecast(),frame,mixed)
    same = fit!(ReservoirForecast(),frame,targets)
    different = fit!(ReservoirForecast(;seed=22),frame,targets)
    @test checkpoint_id(same) == id
    @test checkpoint_id(different) != id
    @test all(iszero, same.reservoir.ensemble.x)
    @test same.encoder.previous === nothing
    @test LC.FEATURE_ORDER == ("spread_ticks","mid_ticks","microprice_ticks","imbalance","signed_order_flow")
    # No field exists through which a hardware pulse can enter.
    @test LC.feature_values(first(frame)) == [2,101,101.1,-0.5,-1]
end

@testset "Ablations and independent scoring oracle" begin
    frame, targets = tiny_frame(), tiny_targets()
    forecasts = []
    for (encoding,temporal) in ((:delta,true),(:raw,true),(:delta,false))
        model = fit!(ReservoirForecast(;encoding,temporal),frame,targets)
        push!(forecasts,[f.probabilities for f in predict!(model,tiny_frame(40),ForecastHorizon(4))])
    end
    @test forecasts[1] != forecasts[2]
    @test forecasts[1] != forecasts[3]
    row = first(frame)
    f = Forecast(ForecastHorizon(4), row, (0.2,0.3,0.5))
    m = LC.forecast_metrics([f],[MovementTarget(row.sequence,row.sequence+4,4,Up)])
    @test m.n == 1
    @test m.accuracy == 1
    @test m.brier ≈ 0.38
    @test m.log_loss ≈ -log(0.5)
    @test m.ece ≈ 0.5
end

@testset "Real campaign, held-out ranges and paper export" begin
    dir = joinpath(mktempdir(),"campaign")
    summary = run_comparison(dir)
    @test length(summary.outcomes) == 18
    for outcome in summary.outcomes
        @test outcome.status == "ok"
        if outcome.status == "ok"
            @test outcome.validation.n == 96
            @test outcome.test.n == 116
            @test outcome.paper["final_inventory_lots"] in (0,1)
        else
            @info outcome
        end
    end
    @test_throws ArgumentError run_comparison(dir)
    manifest = JSON3.read(read(joinpath(dir,"manifest.json"),String))
    @test !manifest.telemetry_features
    @test !manifest.policy.kelly
    @test manifest.tuning_budget_per_candidate == 0
    @test manifest.splits.train.sequences[2] + 4 < manifest.splits.validation.sequences[1]
    @test manifest.splits.validation.sequences[2] + 4 < manifest.splits.test.sequences[1]
    @test isfile(joinpath(dir,"dependencies.toml"))
    for outcome in summary.outcomes
        outcome.status == "ok" || continue
        run = joinpath(dir,"$(outcome.candidate)-s$(outcome.seed)")
        fills = JSON3.read.(readlines(joinpath(run,"fills.jsonl")))
        for fill in fills
            @test fill.exchange_ts_ns >= fill.decision_ts_ns + 200_000
        end
        paper = JSON3.read(read(joinpath(run,"paper.json"),String))
        @test paper.assumptions.latency.order_entry_ns == 200_000
        @test paper.assumptions.fees.taker_fee_ppm == 100
        @test !paper.assumptions.latency.diagnostic_zero_latency
    end
end
