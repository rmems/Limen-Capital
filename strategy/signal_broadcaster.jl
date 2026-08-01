#=
signal_broadcaster.jl — JSON adapter (secondary path)

Primary research wire is binary ReadoutPacket on LIMEN_IPC_PUB (see wire/README.md).
This module is the **JSON IPC adapter** for tools that still emit TradeSignal JSON
(`LIMEN_WIRE=json` on the Rust muscle).

Endpoint (override with `LIMEN_JSON_IPC`, must be `ipc://` + absolute path, no `..`):
  - `$XDG_RUNTIME_DIR/limen-capital/signals.ipc` when set
  - else `/tmp/limen-capital-\$UID/signals.ipc` (mode 0o700; numeric OS UID)

Usage:
    using Revise
    include("strategy/signal_broadcaster.jl")

    # In your main_brain.jl:
    broadcaster = SignalBroadcaster()
    broadcast_trade(broadcaster, "BTC-USD", :Buy, 65000.0, 1.0, 0.98)
=#

using ZMQ
using JSON3
using Printf
using Dates

"""Validate `LIMEN_JSON_IPC`: `ipc://` + absolute path, no `..` segments."""
function validate_json_ipc_endpoint(ep::AbstractString)::String
    ep = strip(ep)
    isempty(ep) && error("LIMEN_JSON_IPC is empty")
    startswith(ep, "ipc://") || error("LIMEN_JSON_IPC must start with ipc://")
    path = ep[7:end]  # after "ipc://"
    startswith(path, "/") || error("LIMEN_JSON_IPC path must be absolute (ipc:///path/...)")
    any(==(".."), split(path, '/')) && error("LIMEN_JSON_IPC must not contain '..' path segments")
    return String(ep)
end

"""Numeric OS UID for multi-user /tmp isolation (Linux /proc, else getuid)."""
function current_os_uid()::UInt32
    try
        for line in eachline("/proc/self/status")
            if startswith(line, "Uid:")
                return parse(UInt32, split(line)[2])
            end
        end
    catch
    end
    try
        return UInt32(Libc.getuid())
    catch
        return UInt32(0)
    end
end

"""Create/verify a non-symlink, owner-only IPC directory (fail closed)."""
function prepare_json_ipc_dir(dir::AbstractString)
    if islink(dir)
        error("JSON IPC path $dir is a symlink — refusing attacker-controlled path")
    end
    try
        mkpath(dir)
    catch e
        error("failed to create JSON IPC directory $dir: $e")
    end
    islink(dir) && error("JSON IPC path $dir is a symlink after create")
    isdir(dir) || error("JSON IPC path $dir is not a directory")
    try
        st = stat(dir)
        st.uid == current_os_uid() || error(
            "JSON IPC directory $dir owned by uid $(st.uid), expected $(current_os_uid())",
        )
        chmod(dir, 0o700)
    catch e
        error("failed to secure JSON IPC directory $dir: $e")
    end
    return nothing
end

"""Default JSON IPC endpoint (UID-scoped; env `LIMEN_JSON_IPC` overrides)."""
function default_json_ipc_endpoint()::String
    if haskey(ENV, "LIMEN_JSON_IPC") && !isempty(ENV["LIMEN_JSON_IPC"])
        # Custom endpoint: do not mkdir the default tree as a side effect.
        return validate_json_ipc_endpoint(ENV["LIMEN_JSON_IPC"])
    end
    if haskey(ENV, "XDG_RUNTIME_DIR") && !isempty(ENV["XDG_RUNTIME_DIR"])
        dir = joinpath(ENV["XDG_RUNTIME_DIR"], "limen-capital")
    else
        dir = joinpath("/tmp", "limen-capital-$(current_os_uid())")
    end
    prepare_json_ipc_dir(dir)
    return "ipc://$(joinpath(dir, "signals.ipc"))"
end

"""
    SignalBroadcaster

Maintains ZMQ publisher socket for broadcasting trade signals to Rust execution engine.
Thread-safe wrapper around ZMQ context and socket.
"""
mutable struct SignalBroadcaster
    context::ZMQ.Context
    socket::ZMQ.Socket
    message_count::Int64
    endpoint::String
    owner_lock_path::Union{Nothing,String}

    function SignalBroadcaster(endpoint::Union{Nothing,String}=nothing)
        context = ZMQ.Context()
        socket = ZMQ.Socket(context, ZMQ.PUB)
        ep = something(endpoint, default_json_ipc_endpoint())
        owner_lock_path = nothing
        # libzmq ipc:// bind will *unlink* an existing path before binding, so a
        # second publisher can steal a live endpoint. Hold an exclusive owner lock
        # *before* bind; only then clear a stale socket file and bind.
        if startswith(ep, "ipc://")
            sock_path = ep[7:end]
            owner_lock_path = acquire_ipc_owner_lock(sock_path)
            if ispath(sock_path)
                # Lock proves no live owner (or we reclaimed a dead lock). Safe
                # to remove a leftover Unix socket so bind does not fail.
                remove_unix_socket_if_present!(sock_path)
            end
        end
        try
            ZMQ.bind(socket, ep)
        catch e
            if owner_lock_path !== nothing
                release_ipc_owner_lock(owner_lock_path)
            end
            error(
                "SignalBroadcaster bind failed at $ep: $e. " *
                "Check parent directory permissions/ownership.",
            )
        end

        println("[$(now())] SignalBroadcaster initialized at $ep")
        new(context, socket, 0, ep, owner_lock_path)
    end
end

"""True if `pid` appears to be a live process (Linux `/proc`, else best-effort)."""
function process_appears_alive(pid::Integer)::Bool
    pid <= 0 && return false
    return isdir("/proc/$(pid)")
end

"""
Exclusive owner lock beside the IPC socket (`path.owner.lock`).

Prevents concurrent publishers: libzmq would otherwise unlink a live socket on
bind. Stale locks from dead PIDs are reclaimed. `LIMEN_JSON_IPC_REPLACE=1` forces
lock reclaim even if a PID file claims a live process (operator override only).
"""
function acquire_ipc_owner_lock(sock_path::AbstractString)::String
    lock_path = sock_path * ".owner.lock"
    force = get(ENV, "LIMEN_JSON_IPC_REPLACE", "0") == "1"
    if ispath(lock_path)
        old = try
            strip(read(lock_path, String))
        catch
            ""
        end
        old_pid = try
            parse(Int, old)
        catch
            nothing
        end
        if old_pid !== nothing && process_appears_alive(old_pid) && !force
            error(
                "IPC endpoint already owned by live pid=$old_pid (lock $lock_path). " *
                "Refusing to steal; stop that broadcaster or set LIMEN_JSON_IPC_REPLACE=1 " *
                "only if you intend to take over.",
            )
        end
        # Stale lock (dead pid) or forced REPLACE.
        try
            rm(lock_path; force=true)
        catch e
            error("Could not clear IPC owner lock $lock_path: $e")
        end
    end
    # Exclusive create (O_WRONLY|O_CREAT|O_EXCL) — Linux research hosts.
    o_wronly = Cint(1)
    o_creat = Cint(64)
    o_excl = Cint(128)
    fd = ccall(
        :open,
        Cint,
        (Cstring, Cint, Cint),
        lock_path,
        o_wronly | o_creat | o_excl,
        0o600,
    )
    if fd < 0
        error(
            "IPC owner lock race at $lock_path (another broadcaster won). " *
            "Endpoint remains exclusive.",
        )
    end
    try
        pid_bytes = Vector{UInt8}(string(getpid()))
        ccall(:write, Cssize_t, (Cint, Ptr{UInt8}, Csize_t), fd, pid_bytes, length(pid_bytes))
    finally
        ccall(:close, Cint, (Cint,), fd)
    end
    return lock_path
end

function release_ipc_owner_lock(lock_path::AbstractString)
    try
        rm(lock_path; force=true)
    catch
        # Best-effort cleanup on bind failure / shutdown.
    end
    return nothing
end

"""Remove a Unix socket at `path` if present; refuse regular files/dirs/symlinks."""
function remove_unix_socket_if_present!(path::AbstractString)
    ispath(path) || return nothing
    islink(path) && error("IPC path $path is a symlink — refusing to delete")
    isdir(path) && error("IPC path $path is a directory — refusing to delete")
    isfile(path) && error(
        "IPC path $path is a regular file — refusing to delete non-socket target",
    )
    # Linux S_IFMT=0o170000, S_IFSOCK=0o140000 — only unlink real Unix sockets.
    mode = stat(path).mode
    if (mode & 0o170000) != 0o140000
        error(
            "IPC path $path is not a Unix socket (mode=$(string(mode; base=8))) — refusing to delete",
        )
    end
    try
        rm(path)
    catch e
        error("Could not remove IPC socket $path: $e")
    end
    return nothing
end

"""
    broadcast_trade(broadcaster::SignalBroadcaster, ticker, side, price, quantity, confidence)

Publish a trade signal to the Rust execution engine.

# Arguments
- `ticker::String`: Asset ticker (e.g., "BTC-USD")
- `side::Symbol`: `:Buy`, `:Sell`, or `:Neutral`
- `price::Float64`: Current price
- `quantity::Float64`: Trade size
- `confidence::Float32`: SNN confidence (0.0 - 1.0)
"""
function broadcast_trade(
    broadcaster::SignalBroadcaster,
    ticker::String,
    side::Symbol,
    price::Float64,
    quantity::Float64,
    confidence::Float32
)
    # Nanosecond precision for latency tracking
    timestamp_ns = Int64(time_ns())

    # Map Julia symbol to JSON string
    side_str = String(side) |> lowercase

    # Construct the signal
    signal = Dict(
        "ticker" => ticker,
        "side" => side_str,
        "price" => price,
        "quantity" => quantity,
        "confidence" => confidence,
        "timestamp_ns" => timestamp_ns
    )

    # Serialize to JSON
    json_str = JSON3.write(signal)

    # Publish over ZMQ (fire-and-forget, no error handling for ultra-low latency)
    try
        ZMQ.send(broadcaster.socket, json_str)
        broadcaster.message_count += 1

        @info "[SIGNAL] $ticker $(side_str) $(quantity)@\$$(price) (confidence: $(confidence), latency_tracking: $(timestamp_ns))"
    catch e
        @warn "Failed to broadcast signal: $e"
    end
end

"""
    shutdown(broadcaster::SignalBroadcaster)

Gracefully close ZMQ socket and context.
"""
function shutdown(broadcaster::SignalBroadcaster)
    try
        ZMQ.close(broadcaster.socket)
        ZMQ.term(broadcaster.context)
        if broadcaster.owner_lock_path !== nothing
            release_ipc_owner_lock(broadcaster.owner_lock_path)
            broadcaster.owner_lock_path = nothing
        end
        @info "SignalBroadcaster shut down (sent $(broadcaster.message_count) signals)"
    catch e
        @warn "Error during shutdown: $e"
    end
end

# Convenience function for testing
function test_broadcast()
    println("\n=== Testing Signal Broadcaster ===")
    broadcaster = SignalBroadcaster()

    # Simulate a few signals
    broadcast_trade(broadcaster, "BTC-USD", :Buy, 65000.0, 1.0, 0.98f0)
    sleep(0.1)
    broadcast_trade(broadcaster, "ETH-USD", :Buy, 3500.0, 5.0, 0.87f0)
    sleep(0.1)
    broadcast_trade(broadcaster, "BTC-USD", :Neutral, 65000.0, 0.0, 0.50f0)

    println("\nSignals sent. Rust execution engine should receive and process them.")
    println("Keep Rust engine running to see signals arrive.\n")

    shutdown(broadcaster)
end

# Export public API
export SignalBroadcaster, broadcast_trade, shutdown, test_broadcast, default_json_ipc_endpoint, validate_json_ipc_endpoint
