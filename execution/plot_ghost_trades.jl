#!/usr/bin/env julia

"""
plot_ghost_trades.jl — Turn ghost trading logs into publication-ready plots.

Usage:
    julia spikenaut-capital/execution/plot_ghost_trades.jl \
        [input_jsonl] [output_dir]

Defaults:
    input_jsonl = "domain/research/ghost_market_log.jsonl"
    output_dir  = "research/plots/trading"

Outputs:
    • equity_curve.png        — portfolio value vs. cash balance timeline
    • pnl_curve.png           — cumulative PnL progression
    • asset_heatmap.png       — per-asset wallet holdings heatmap
    • spike_activity.png      — buy/sell intensity per step (proxy for SNN spikes)
    • summary.md              — markdown summary for social posts / grant decks

Packages: JSON3, DataFrames, Dates, Statistics, Plots
Install (once):
    julia -e 'using Pkg; Pkg.add.("JSON3","DataFrames","Plots")'
"""

using JSON3
using DataFrames
using Dates
using Statistics
using Plots

const DEFAULT_INPUT = "domain/research/ghost_market_log.jsonl"
const DEFAULT_OUTPUT_DIR = "research/plots/trading"
const TS_FMT = dateformat"yyyy-mm-ddTHH:MM:SS.sssssssss"

function parse_timestamp(ts::AbstractString)
    stripped = first(split(ts, '+'))
    try
        return DateTime(stripped, TS_FMT)
    catch err
        @warn "Failed to parse timestamp" ts err
        return DateTime(stripped[1:19], dateformat"yyyy-mm-ddTHH:MM:SS")
    end
end

function read_jsonl(path::AbstractString)
    rows = Vector{Dict{Symbol,Any}}()
    open(path, "r") do io
        for (idx, line) in enumerate(eachline(io))
            stripped = strip(line)
            isempty(stripped) && continue
            obj = JSON3.read(stripped)
            data = Dict{Symbol,Any}()
            for (k, v) in pairs(obj)
                data[k] = v
            end
            data[:__line] = idx
            push!(rows, data)
        end
    end
    return DataFrame(rows)
end

function ensure_column!(df::DataFrame, col::Symbol, fallback)
    hasproperty(df, col) && return
    df[!, col] = fill(fallback, nrow(df))
end

function prepare_frame(df::DataFrame)
    isempty(df) && error("No records were loaded from ghost trade log.")

    if :timestamp in names(df)
        df[!, :timestamp_dt] = parse_timestamp.(String.(df.timestamp))
    else
        base_time = now()
        df[!, :timestamp_dt] = base_time .+ Second.(df.step .- df.step[1])
    end

    for col in (:portfolio_value, :balance_usdt, :cumulative_pnl)
        ensure_column!(df, col, 0.0)
    end

    asset_cols = (:balance_dnx, :balance_quai, :balance_qubic, :balance_kaspa,
                  :balance_monero, :balance_ocean, :balance_verus)
    for col in asset_cols
        ensure_column!(df, col, 0.0)
    end

    df[!, :portfolio_value] = coalesce.(df.portfolio_value, df.balance_usdt)
    df[!, :balance_usdt] = coalesce.(df.balance_usdt, 0.0)
    df[!, :cumulative_pnl] = coalesce.(df.cumulative_pnl, 0.0)
    df[!, :invested_capital] = df.portfolio_value .- df.balance_usdt

    sort!(df, [:timestamp_dt, :step])
    return df
end

function plot_equity(df::DataFrame, out_dir::AbstractString)
    plt = plot(df.timestamp_dt, df.portfolio_value,
        label = "Portfolio Value",
        linewidth = 2,
        color = :turquoise4,
        xlabel = "Time",
        ylabel = "USD",
        title = "Spikenaut Equity Curve",
        legend = :topleft,
        background_color = RGB(0.05,0.05,0.08),
        foreground_color = :white,
        grid = :both)

    plot!(plt, df.timestamp_dt, df.balance_usdt,
        label = "Cash Reserve",
        linewidth = 2,
        color = :gold)

    plot!(plt, df.timestamp_dt, df.invested_capital,
        label = "Deployed Capital",
        linewidth = 2,
        color = :orchid4)

    savefig(plt, joinpath(out_dir, "equity_curve.png"))
end

function plot_pnl(df::DataFrame, out_dir::AbstractString)
    plt = plot(df.timestamp_dt, df.cumulative_pnl,
        label = "Cumulative PnL",
        linewidth = 2,
        color = :springgreen,
        xlabel = "Time",
        ylabel = "USD",
        title = "PnL Progression",
        background_color = RGB(0.05,0.05,0.08),
        foreground_color = :white,
        grid = :both,
        legend = :bottomright)
    hline!(plt, [0.0], color = :gray, linestyle = :dash, label = "Break-even")
    savefig(plt, joinpath(out_dir, "pnl_curve.png"))
end

function plot_asset_heatmap(df::DataFrame, out_dir::AbstractString)
    assets = [:balance_dnx, :balance_quai, :balance_qubic, :balance_kaspa,
              :balance_monero, :balance_ocean, :balance_verus]
    asset_labels = ["DNX","QUAI","QUBIC","KAS","XMR","OCEAN","VERUS"]
    mat = reduce(hcat, [Float64.(df[!, col]) for col in assets])
    # Normalize per-asset to highlight rotation
    mat_norm = similar(mat)
    for j in 1:size(mat, 2)
        col = mat[:, j]
        rng = maximum(col) - minimum(col)
        mat_norm[:, j] = rng > 0 ? (col .- minimum(col)) ./ rng : zeros(length(col))
    end

    plt = heatmap(asset_labels, df.timestamp_dt, mat_norm,
        xlabel = "Asset",
        ylabel = "Time",
        c = :plasma,
        title = "Wallet Allocation Heatmap (normalized holdings)")
    savefig(plt, joinpath(out_dir, "asset_heatmap.png"))
end

function plot_spike_activity(df::DataFrame, out_dir::AbstractString)
    grouped = combine(groupby(df, :step),
        :timestamp_dt => first => :timestamp_dt,
        :action => (acts -> count(==("buy"), acts)) => :buy_count,
        :action => (acts -> count(==("sell"), acts)) => :sell_count)

    plt = bar(grouped.timestamp_dt,
        [grouped.buy_count grouped.sell_count],
        label = ["Buy Spikes" "Sell Spikes"],
        xlabel = "Time",
        ylabel = "Count / tick",
        title = "SNN Spike Activity (trade triggers)",
        bar_position = :stack,
        color = [:mediumspringgreen :tomato],
        background_color = RGB(0.05,0.05,0.08),
        foreground_color = :white)

    savefig(plt, joinpath(out_dir, "spike_activity.png"))
end

function write_summary(df::DataFrame, out_dir::AbstractString)
    equity = df.portfolio_value
    cash = df.balance_usdt
    pnl = df.cumulative_pnl
    start_val = first(equity)
    end_val = last(equity)
    return_pct = isempty(equity) ? 0.0 : ((end_val - start_val) / max(start_val, 1e-6)) * 100
    buy_count = count(==("buy"), df.action)
    sell_count = count(==("sell"), df.action)
    total_trades = buy_count + sell_count
    session_hours = (df.timestamp_dt[end] - df.timestamp_dt[1]) / Hour(1)

    final_assets = Dict(
        "DNX"   => last(df.balance_dnx),
        "QUAI"  => last(df.balance_quai),
        "QUBIC" => last(df.balance_qubic),
        "KAS"   => last(df.balance_kaspa),
        "XMR"   => last(df.balance_monero),
        "OCEAN" => last(df.balance_ocean),
        "VERUS" => last(df.balance_verus),
    )

    summary_path = joinpath(out_dir, "summary.md")
    open(summary_path, "w") do io
        println(io, "# Spikenaut Trading Session Summary")
        println(io,)
        println(io, "- **Session Window:** $(df.timestamp_dt[1]) → $(df.timestamp_dt[end]) ($(round(session_hours, digits=2)) h)")
        println(io, "- **Starting Equity:** \$$(round(start_val, digits=2))")
        println(io, "- **Ending Equity:** \$$(round(end_val, digits=2))")
        println(io, "- **Return:** $(round(return_pct, digits=2))%")
        println(io, "- **Cumulative PnL:** \$$(round(last(pnl), digits=2))")
        println(io, "- **Cash on Hand:** \$$(round(last(cash), digits=2))")
        println(io, "- **Trades:** $total_trades (buys=$buy_count, sells=$sell_count)")
        println(io, "- **Peak Equity:** \$$(round(maximum(equity), digits=2))")
        println(io, "- **Drawdown Floor:** \$$(round(minimum(equity), digits=2))")
        println(io, "- **Final Wallet Mix:**")
        for (asset, amt) in final_assets
            println(io, "  - $asset = $(round(amt, digits=4)) units")
        end
        println(io)
        println(io, "Generated plots: equity_curve.png, pnl_curve.png, asset_heatmap.png, spike_activity.png")
    end
end

function main()
    input_path = length(ARGS) >= 1 ? ARGS[1] : DEFAULT_INPUT
    output_dir = length(ARGS) >= 2 ? ARGS[2] : DEFAULT_OUTPUT_DIR

    isfile(input_path) || error("Input file not found: $input_path")
    isdir(output_dir) || mkpath(output_dir)

    println("[plotter] Loading ghost trades from $input_path …")
    df_raw = read_jsonl(input_path)
    df = prepare_frame(df_raw)
    println("[plotter] Loaded $(nrow(df)) records spanning $(df.timestamp_dt[1]) → $(df.timestamp_dt[end])")

    plot_equity(df, output_dir)
    println("[plotter] Wrote equity_curve.png")
    plot_pnl(df, output_dir)
    println("[plotter] Wrote pnl_curve.png")
    plot_asset_heatmap(df, output_dir)
    println("[plotter] Wrote asset_heatmap.png")
    plot_spike_activity(df, output_dir)
    println("[plotter] Wrote spike_activity.png")
    write_summary(df, output_dir)
    println("[plotter] Summary ready → $(joinpath(output_dir, "summary.md"))")
end

main()
