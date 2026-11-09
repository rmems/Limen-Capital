const CAMPAIGN_SEEDS = (11, 22, 33)
const CAMPAIGN_HORIZON = 4
const POLICY_THRESHOLD = 0.6

"""Synthetic plumbing fixture, independent of model seed and model feedback."""
function fixture_records()
    rng = MersenneTwister(9001)
    records = NamedTuple[]
    function add(side, price, size)
        seq = length(records) + 1
        push!(records, (type="book_delta", venue="SIM", instrument="SYNTH",
            exchange_ts_ns=seq * 1_000_000, receive_ts_ns=seq * 1_000_000 + 100_000,
            sequence=seq, side=side, price_ticks=price, size_delta=size))
    end
    add("BID", 90, 100)
    add("ASK", 110, 100)
    bid, ask, bs, ass = 99, 101, 10, 10
    add("BID", bid, bs)
    add("ASK", ask, ass)
    for _ in 1:120
        add("BID", bid, -bs)
        add("ASK", ask, -ass)
        mid = rand(rng, 96:104)
        bid, ask, bs, ass = mid - 1, mid + 1, rand(rng, 5:20), rand(rng, 5:20)
        add("BID", bid, bs)
        add("ASK", ask, ass)
    end
    records
end

writejson(path, x) = open(io -> JSON3.write(io, x), path, "w")
function writejsonl(path, rows)
    open(path, "w") do io
        for row in rows
            JSON3.write(io, row)
            println(io)
        end
    end
end
filehash(path) = bytes2hex(sha256(read(path)))
plain(x) = Dict(string(k) => getfield(x, k) for k in fieldnames(typeof(x)))

function forecast_metrics(forecasts, targets)
    labels = Dict(t.anchor_sequence => findfirst(==(t.label), CLASS_ORDER) for t in targets)
    matched = [(f, labels[f.sequence]) for f in forecasts if haskey(labels, f.sequence)]
    isempty(matched) && throw(ArgumentError("no evaluable forecasts"))
    brier, loss, correct = 0.0, 0.0, 0
    bins = [Dict("n"=>0, "confidence_sum"=>0.0, "correct"=>0) for _ in 1:5]
    for (f, label) in matched
        p = f.probabilities
        brier += sum((p[j] - (j == label))^2 for j in 1:3)
        # Declared numerical log floor, not fitted calibration.
        loss -= log(max(p[label], 1e-15))
        hit = argmax(p) == label
        correct += hit
        confidence = maximum(p)
        bin = bins[min(5, floor(Int, confidence * 5) + 1)]
        bin["n"] += 1
        bin["confidence_sum"] += confidence
        bin["correct"] += hit
    end
    n = length(matched)
    ece = sum(abs(b["correct"] - b["confidence_sum"]) for b in bins) / n
    (n=n, accuracy=correct/n, brier=brier/n, log_loss=loss/n, ece=ece, reliability_bins=bins,
        class_counts=[count(x -> x[2] == j, matched) for j in 1:3])
end

"""Predict on every eligible event, including while an order report is pending."""
struct ComparisonPolicy{M}
    model::M
    first_sequence::Int64
    last_sequence::Int64
    forecasts::Vector{Forecast}
end

function (p::ComparisonPolicy)(ctx::DecisionContext)
    p.first_sequence <= ctx.info_sequence <= p.last_sequence || return nothing
    ctx.features === nothing && return nothing
    f = only(predict!(p.model, FeatureFrame([ctx.features]), ForecastHorizon(CAMPAIGN_HORIZON)))
    push!(p.forecasts, f)
    isempty(ctx.open_orders) || return nothing
    down, _, up = f.probabilities
    if up >= POLICY_THRESHOLD && ctx.reported_inventory_lots == 0
        return OrderIntent(Buy, 1, first(first(ctx.snapshot.asks)); time_in_force=:ioc)
    elseif down >= POLICY_THRESHOLD && ctx.reported_inventory_lots == 1
        return OrderIntent(Sell, 1, first(first(ctx.snapshot.bids)); time_in_force=:ioc)
    end
    nothing
end

candidate(name, seed) =
    name == "StationaryModel" ? StationaryModel() :
    name == "ImbalanceRule" ? ImbalanceRule(0.0) :
    name == "RidgeClassifier" ? RidgeClassifier(1.0) :
    ReservoirForecast(; seed, encoding=name == "reservoir_raw" ? :raw : :delta,
        temporal=name != "reservoir_no_memory")

const CANDIDATES = ("StationaryModel", "ImbalanceRule", "RidgeClassifier",
    "reservoir_delta", "reservoir_raw", "reservoir_no_memory")

"""Frozen, zero-search CPU comparison. All outcomes (including failures) are retained."""
function run_comparison(output_dir; data_path=nothing)
    ispath(output_dir) && throw(ArgumentError("output directory must be new"))
    mkpath(output_dir)
    qualification = qualify_backend()
    input_path = joinpath(output_dir, "events.jsonl")
    if data_path === nothing
        writejsonl(input_path, fixture_records())
    else
        cp(data_path, input_path)
    end
    session = load_session_jsonl(input_path)
    snapshots = replay!(OrderBookState(session.venue, session.instrument), session)
    frame = microstructure_features(snapshots)
    split = first(walk_forward_splits(length(frame); train_size=240, validation_size=100,
        test_size=120, embargo=4, max_horizon=4))
    ranges = (train=split.train, validation=split.validation, test=split.test)
    frames = Dict(k => FeatureFrame(collect(frame[r])) for (k,r) in pairs(ranges))
    # Label each range in isolation, so trailing anchors cannot reach the next split.
    targets = Dict(k => label_event_horizon(
        [s for s in snapshots if first(frames[k]).sequence <= s.sequence <= last(frames[k]).sequence];
        horizon_events=CAMPAIGN_HORIZON) for k in keys(frames))
    for k in keys(frames)
        writejsonl(joinpath(output_dir,"$(k)-features.jsonl"), frames[k])
        writejsonl(joinpath(output_dir,"$(k)-targets.jsonl"), targets[k])
    end
    paper_session = ReplaySession([e for e in session if e.sequence <= last(frames[:test]).sequence])
    config = SimulationConfig(; latency=FixedLatency(200_000),
        fees=FeeSchedule(; maker_fee_ppm=100, taker_fee_ppm=100),
        fill_model=L2QueueApproximation(; seed=9001, taker_slippage_ticks=0),
        limits=RiskLimits(; max_order_lots=1, max_inventory_lots=1, max_open_orders=1))
    boundary = Dict(string(k) => (rows=[first(r),last(r)],
        sequences=[first(frames[k]).sequence,last(frames[k]).sequence],
        exchange_ts_ns=[first(frames[k]).exchange_ts_ns,last(frames[k]).exchange_ts_ns],
        n_features=length(frames[k]), n_labels=length(targets[k])) for (k,r) in pairs(ranges))
    # Write the frozen choices before any held-out prediction.
    manifest = Dict{String,Any}(
        "schema"=>"limen.cpu-comparison.v1", "qualification"=>qualification,
        "data_sha256"=>filehash(input_path), "data_source"=>data_path === nothing ? "synthetic fixture seed 9001" : abspath(data_path),
        "julia_version"=>string(VERSION), "threads"=>Threads.nthreads(),
        "environment"=>(kernel=Sys.KERNEL, architecture=Sys.ARCH, cpu=Sys.CPU_NAME),
        "blas_threads"=>BLAS.get_num_threads(), "feature_order"=>FEATURE_ORDER,
        "classes"=>["Down","Flat","Up"], "horizon_events"=>CAMPAIGN_HORIZON,
        "seeds"=>CAMPAIGN_SEEDS, "candidates"=>CANDIDATES, "splits"=>boundary,
        "embargo_events"=>4, "tuning_budget_per_candidate"=>0,
        "n_input_events"=>length(session), "n_input_features"=>length(frame),
        "unused_trailing_features"=>length(frame)-last(split.test),
        "calibration"=>"none; report raw direction-class probabilities",
        "policy"=>(threshold=POLICY_THRESHOLD, order_lots=1, max_inventory_lots=1,
            mode="long_flat", kelly=false, initial_cash_subunits=100_000),
        "encoder"=>(threshold=0.05, saturation=1, scaling="train max absolute"),
        "reset"=>"zero reservoir/encoder before fit, validation, test, and paper replay; no warmup",
        "telemetry_features"=>false, "hardware_confound_experiment"=>false,
        "limitations"=>["synthetic data only by default", "one chronological split",
            "tanh reservoir, not LIF qualification", "L2 fills approximate; no market impact",
            "LIM-1035 exporter pending; reuse decision_records/fill_records/audit via paper simulator",
            "no profitability, production-HFT or Spikenaut generalization claim"])
    root = dirname(@__DIR__)
    manifest["simulation"] = (order_entry_ns=200_000, cancel_ns=200_000,
        report_ns=200_000, feed="ReceiveTimestampFeed", queue_position="back",
        fill_seed=9001, taker_slippage_ticks=0, maker_fee_ppm=100, taker_fee_ppm=100,
        per_fill_fee_subunits=0, qty_scale=1, positive_fees="cost; commission rounds up")
    manifest["code_revision"] = readchomp(`git -C $root rev-parse HEAD`)
    manifest["source_sha256"] = Dict(p=>filehash(joinpath(root,p)) for p in
        ("Project.toml", "src/LimenCapital.jl", "src/forecast_adapter.jl",
         "src/comparison.jl", "brain/reservoir.jl"))
    dependency_manifest = joinpath(root, "Manifest.toml")
    isfile(dependency_manifest) || error("instantiate first to record resolved dependencies")
    cp(dependency_manifest, joinpath(output_dir, "dependencies.toml"))
    manifest["dependencies_sha256"] = filehash(dependency_manifest)
    writejson(joinpath(output_dir, "manifest.json"), manifest)
    outcomes = Any[]
    for name in CANDIDATES, seed in CAMPAIGN_SEEDS
        id = "$name-s$seed"
        dir = joinpath(output_dir,id)
        mkpath(dir)
        try
            model = candidate(name,seed)
            fit!(model, frames[:train], targets[:train])
            identity = model isa ReservoirForecast ? checkpoint_id(model) :
                bytes2hex(sha256(JSON3.write(plain(model))))
            reset_state!(model)
            validation = forecast_metrics(predict!(model, frames[:validation],
                ForecastHorizon(CAMPAIGN_HORIZON)), targets[:validation])
            # No selection or tuning based on validation; every preregistered candidate proceeds.
            reset_state!(model)
            forecasts = predict!(model, frames[:test], ForecastHorizon(CAMPAIGN_HORIZON))
            test = forecast_metrics(forecasts, targets[:test])
            writejsonl(joinpath(dir,"forecasts.jsonl"), forecasts)
            reset_state!(model)
            policy = ComparisonPolicy(model, first(frames[:test]).sequence,
                last(frames[:test]).sequence, Forecast[])
            result = simulate!(PaperPortfolio(session.instrument,100_000), paper_session, policy; config)
            # Same causal feature and reset contract through the actual paper entry point.
            [f.probabilities for f in forecasts] == [f.probabilities for f in policy.forecasts] ||
                error("offline and simulator forecasts diverged")
            isempty(reconciliation_errors(result)) || error("paper accounting did not reconcile")
            writejsonl(joinpath(dir,"decisions.jsonl"), decision_records(result))
            writejsonl(joinpath(dir,"fills.jsonl"), fill_records(result))
            writejsonl(joinpath(dir,"audit.jsonl"), audit_dict.(result.audit))
            writejsonl(joinpath(dir,"ledger.jsonl"), plain.(result.ledger))
            writejson(joinpath(dir,"paper.json"), (metrics=plain(result.metrics),
                assumptions=result.assumptions, result_hash=result.result_hash))
            push!(outcomes,(candidate=name, seed=seed, status="ok", checkpoint=identity,
                validation=validation, test=test, paper=plain(result.metrics)))
        catch e
            e isa InterruptException && rethrow()
            push!(outcomes,(candidate=name, seed=seed, status="failed", error=sprint(showerror,e)))
        end
    end
    aggregates = Dict{String,Any}()
    for name in CANDIDATES
        ok = [o for o in outcomes if o.candidate == name && o.status == "ok"]
        aggregates[name] = Dict("completed"=>length(ok), "failed"=>3-length(ok),
            "test"=>Dict(string(metric)=>(mean=mean([getproperty(o.test,metric) for o in ok]),
                std=length(ok)>1 ? std([getproperty(o.test,metric) for o in ok]) : 0.0)
                for metric in (:accuracy,:brier,:log_loss,:ece) if !isempty(ok)),
            "paper"=>Dict(metric=>(mean=mean([o.paper[metric] for o in ok]),
                std=length(ok)>1 ? std([o.paper[metric] for o in ok]) : 0.0)
                for metric in ("fills","fees_subunits","turnover_subunits",
                    "final_equity_subunits","max_drawdown_subunits") if !isempty(ok)))
    end
    summary = (outcomes=outcomes, aggregates=aggregates)
    writejson(joinpath(output_dir,"summary.json"),summary)
    artifacts = Dict(relpath(joinpath(d,f),output_dir)=>filehash(joinpath(d,f))
        for (d,_,fs) in walkdir(output_dir) for f in fs)
    writejson(joinpath(output_dir,"artifact_hashes.json"),artifacts)
    return summary
end
