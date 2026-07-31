# vault_recorder.jl — DuckDB + Polars market/spike storage
#
# High-speed local gather for research ticks. Not live capital vaulting.

using Polars
using DuckDB
using DataFrames
using Dates

# Override with LIMEN_VAULT_DIR (absolute or relative path).
const VAULT_DIR = let
    env = get(ENV, "LIMEN_VAULT_DIR", "")
    if !isempty(env)
        abspath(env)
    else
        abspath(joinpath(@__DIR__, "..", "data", "vault"))
    end
end
const DB_PATH = joinpath(VAULT_DIR, "limen_vault.db")

mutable struct VaultRecorder
    db::DuckDB.DB
    con::DuckDB.Connection
    market_buffer::Vector{Any}
    spike_buffer::Vector{Any}
    batch_size::Int
    tick_count::Int64
end

"""
    VaultRecorder(batch_size=100)
Initialize the DuckDB storage and Polars buffers.
"""
function VaultRecorder(batch_size::Int=100)
    # Ensure directory exists
    mkpath(VAULT_DIR)
    
    db = DuckDB.DB(DB_PATH)
    con = DuckDB.connect(db)
    
    # Initialize DuckDB tables if they don't exist
    DuckDB.execute(con, """
        CREATE TABLE IF NOT EXISTS market_events (
            tick_id BIGINT PRIMARY KEY,
            timestamp_ns UBIGINT,
            dnx_price FLOAT, dnx_vol FLOAT,
            quai_price FLOAT, quai_vol FLOAT,
            qubic_price FLOAT, qubic_vol FLOAT,
            kaspa_price FLOAT, kaspa_vol FLOAT,
            monero_price FLOAT, monero_vol FLOAT,
            ocean_price FLOAT, ocean_vol FLOAT,
            verus_price FLOAT, verus_vol FLOAT,
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
    
    VaultRecorder(db, con, [], [], batch_size, 0)
end

"""
    record_tick!(vault, pulse::MarketPulse, ensemble::EnsembleBrain)
Accumulates a single tick's market and spike data into Polars buffers.
"""
function record_tick!(vr::VaultRecorder, pulse::MarketPulse, ensemble::EnsembleBrain)
    vr.tick_count += 1
    
    # 1. Store Market Data
    push!(vr.market_buffer, (
        Int64(vr.tick_count),
        pulse.timestamp_ns,
        pulse.dnx_price, pulse.dnx_vol,
        pulse.quai_price, pulse.quai_vol,
        pulse.qubic_price, pulse.qubic_vol,
        pulse.kaspa_price, pulse.kaspa_vol,
        pulse.monero_price, pulse.monero_vol,
        pulse.ocean_price, pulse.ocean_vol,
        pulse.verus_price, pulse.verus_vol,
        pulse.confidence_signal,
        pulse.gpu_temp_c
    ))
    
    # 2. Store Spike Events (Sparse Extraction)
    for (li, lobe) in enumerate(ensemble.lobes)
        # Find indices of spiked neurons on GPU and bring to CPU
        # S is 0.0 or 1.0
        spiked = Array(findall(lobe.S .> 0.5f0))
        for sid in spiked
            push!(vr.spike_buffer, (
                Int64(vr.tick_count),
                Int32(sid - 1),
                Int8(li)
            ))
        end
    end
    
    # 3. Periodic Commit to DuckDB
    if vr.tick_count % vr.batch_size == 0
        flush_vault!(vr)
    end
end

"""
    flush_vault!(vault)
Converts buffers to Polars DataFrames and appends to DuckDB.
"""
function flush_vault!(vr::VaultRecorder)
    if isempty(vr.market_buffer)
        return
    end
    
    println("[vault] Flushing batch at tick $(vr.tick_count)...")
    
    # ── Market Data ────────────────────────────────────────────────────────
    df_market = DataFrame(vr.market_buffer)
    # Convert to Polars for extra speed if needed, but DuckDB.register works with DataFrames
    DuckDB.register_data_frame(vr.con, df_market, "temp_market")
    DuckDB.execute(vr.con, "INSERT INTO market_events SELECT * FROM temp_market")
    DuckDB.unregister_data_frame(vr.con, "temp_market")
    empty!(vr.market_buffer)
    
    # ── Spike Data ─────────────────────────────────────────────────────────
    if !isempty(vr.spike_buffer)
        df_spikes = DataFrame(vr.spike_buffer)
        DuckDB.register_data_frame(vr.con, df_spikes, "temp_spikes")
        DuckDB.execute(vr.con, "INSERT INTO spike_events SELECT * FROM temp_spikes")
        DuckDB.unregister_data_frame(vr.con, "temp_spikes")
        empty!(vr.spike_buffer)
    end
    
    println("[vault] batch committed to $(DB_PATH)")
end

"""
    close_vault!(vault)
"""
function close_vault!(vr::VaultRecorder)
    flush_vault!(vr)
    DuckDB.disconnect(vr.con)
    # DuckDB.close(vr.db) # Close DB when completely done
    println("[vault] Spikenaut-Vault closed gracefully.")
end

println("[vault] vault_recorder.jl loading complete")
