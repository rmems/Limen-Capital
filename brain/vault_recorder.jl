# vault_recorder.jl — DuckDB + DataFrames market/spike storage (long-form quotes)

using DuckDB
using DataFrames
using Dates

const VAULT_DIR = let
    env = get(ENV, "LIMEN_VAULT_DIR", "")
    !isempty(env) ? abspath(env) : abspath(joinpath(@__DIR__, "..", "data", "vault"))
end
const DB_PATH = joinpath(VAULT_DIR, "limen_vault.db")

mutable struct VaultRecorder
    db::DuckDB.DB
    con::DuckDB.Connection
    quote_buffer::Vector{Any}
    meta_buffer::Vector{Any}
    spike_buffer::Vector{Any}
    batch_size::Int
    tick_count::Int64
end

function VaultRecorder(batch_size::Int=100)
    mkpath(VAULT_DIR)
    db = DuckDB.DB(DB_PATH)
    con = DuckDB.connect(db)
    DuckDB.execute(con, """
        CREATE TABLE IF NOT EXISTS market_quotes (
            tick_id BIGINT,
            timestamp_ns UBIGINT,
            symbol_idx INTEGER,
            price FLOAT,
            vol FLOAT
        )
    """)
    DuckDB.execute(con, """
        CREATE TABLE IF NOT EXISTS market_tick_meta (
            tick_id BIGINT PRIMARY KEY,
            timestamp_ns UBIGINT,
            confidence FLOAT,
            gpu_temp FLOAT
        )
    """)
    DuckDB.execute(con, """
        CREATE TABLE IF NOT EXISTS spike_events (
            tick_id BIGINT,
            neuron_id INTEGER,
            lobe_id TINYINT
        )
    """)
    VaultRecorder(db, con, [], [], [], batch_size, 0)
end

function record_tick!(vr::VaultRecorder, pulse::MarketPulse, ensemble::EnsembleBrain)
    vr.tick_count += 1
  tid = Int64(vr.tick_count)
    for (idx, (px, vol)) in enumerate(zip(pulse.prices, pulse.vols))
        push!(vr.quote_buffer, (tid, pulse.timestamp_ns, Int32(idx - 1), px, vol))
    end
    push!(vr.meta_buffer, (tid, pulse.timestamp_ns, pulse.confidence_signal, pulse.gpu_temp_c))
    for (li, lobe) in enumerate(ensemble.lobes)
        for sid in Array(findall(lobe.S .> 0.5f0))
            push!(vr.spike_buffer, (tid, Int32(sid - 1), Int8(li)))
        end
    end
    vr.tick_count % vr.batch_size == 0 && flush_vault!(vr)
end

function flush_vault!(vr::VaultRecorder)
    if !isempty(vr.quote_buffer)
        df_q = DataFrame(vr.quote_buffer, [:tick_id, :timestamp_ns, :symbol_idx, :price, :vol])
        DuckDB.register_data_frame(vr.con, df_q, "temp_quotes")
        DuckDB.execute(vr.con, "INSERT INTO market_quotes SELECT * FROM temp_quotes")
        DuckDB.unregister_data_frame(vr.con, "temp_quotes")
        empty!(vr.quote_buffer)
    end
    if !isempty(vr.meta_buffer)
        df_m = DataFrame(vr.meta_buffer, [:tick_id, :timestamp_ns, :confidence, :gpu_temp])
        DuckDB.register_data_frame(vr.con, df_m, "temp_meta")
        DuckDB.execute(vr.con, "INSERT INTO market_tick_meta SELECT * FROM temp_meta")
        DuckDB.unregister_data_frame(vr.con, "temp_meta")
        empty!(vr.meta_buffer)
    end
    if !isempty(vr.spike_buffer)
        df_s = DataFrame(vr.spike_buffer, [:tick_id, :neuron_id, :lobe_id])
        DuckDB.register_data_frame(vr.con, df_s, "temp_spikes")
        DuckDB.execute(vr.con, "INSERT INTO spike_events SELECT * FROM temp_spikes")
        DuckDB.unregister_data_frame(vr.con, "temp_spikes")
        empty!(vr.spike_buffer)
    end
end

function close_vault!(vr::VaultRecorder)
    flush_vault!(vr)
    DBInterface.close!(vr.con)
    DBInterface.close!(vr.db)
end
