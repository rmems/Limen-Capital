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
    owner_lock_fd::Cint  # held open for process lifetime (-1 if unused)

    function SignalBroadcaster(endpoint::Union{Nothing,String}=nothing)
        context = ZMQ.Context()
        local socket = nothing
        owner_lock_path = nothing
        owner_lock_fd = Cint(-1)
        try
            socket = ZMQ.Socket(context, ZMQ.PUB)
            # Do not eagerly evaluate default_json_ipc_endpoint when endpoint is set.
            ep = if endpoint === nothing
                default_json_ipc_endpoint()
            else
                validate_json_ipc_endpoint(endpoint)
            end
            # libzmq ipc:// bind unlinks an existing path first — exclusive flock
            # before bind prevents a second publisher from stealing a live endpoint.
            if startswith(ep, "ipc://")
                sock_path = ep[7:end]
                owner_lock_path, owner_lock_fd = acquire_ipc_owner_lock(sock_path)
                try
                    if ispath(sock_path)
                        remove_unix_socket_if_present!(sock_path)
                    end
                catch e
                    release_ipc_owner_lock(owner_lock_path, owner_lock_fd)
                    owner_lock_path = nothing
                    owner_lock_fd = Cint(-1)
                    rethrow(e)
                end
            end
            try
                ZMQ.bind(socket, ep)
            catch e
                release_ipc_owner_lock(owner_lock_path, owner_lock_fd)
                owner_lock_path = nothing
                owner_lock_fd = Cint(-1)
                error(
                    "SignalBroadcaster bind failed at $ep: $e. " *
                    "Check parent directory permissions/ownership.",
                )
            end
            println("[$(now())] SignalBroadcaster initialized at $ep")
            new(context, socket, 0, ep, owner_lock_path, owner_lock_fd)
        catch e
            if socket !== nothing
                try
                    ZMQ.close(socket)
                catch
                end
            end
            try
                ZMQ.term(context)
            catch
            end
            release_ipc_owner_lock(owner_lock_path, owner_lock_fd)
            rethrow(e)
        end
    end
end

"""
Open flags for the IPC owner-lock sidecar (portable O_CREAT; Linux adds O_CLOEXEC|O_NOFOLLOW).
"""
function ipc_owner_lock_open_flags()::Cint
    # O_RDWR is 2 on Linux, Darwin, and FreeBSD.
    o_rdwr = Cint(2)
    # O_CREAT: Linux 0o100 (64); Darwin/BSD 0x200 (512).
    o_creat = if Sys.islinux()
        Cint(64)
    elseif Sys.isapple() || Sys.isbsd()
        Cint(0x200)
    else
        Cint(64)
    end
    flags = o_rdwr | o_creat
    if Sys.islinux()
        # O_CLOEXEC=0o2000000, O_NOFOLLOW=0o400000 — atomic vs post-open fcntl races.
        flags |= Cint(0o2000000) | Cint(0o400000)
    end
    return flags
end

"""
Acquire exclusive IPC ownership via `flock(LOCK_EX|LOCK_NB)` on `path.owner.lock`.

The file descriptor is held open for the broadcaster lifetime so the lock ends
with the process (kernel drops flock on last close). The lock *file* is never
unlinked on release: ownership is the exclusive flock on a stable inode, not
path existence. Deleting the path after unlock races with a second opener and
can leave two owners on different inodes for the same endpoint name.

Cooperative protocol among Capital publishers only; non-flock binders are outside
this ownership contract (libzmq may still unlink-on-bind against them).
"""
function acquire_ipc_owner_lock(sock_path::AbstractString)::Tuple{String,Cint}
    lock_path = sock_path * ".owner.lock"
    # Refuse symlink targets before open (all Unix); Linux also uses O_NOFOLLOW.
    islink(lock_path) && error(
        "IPC owner lock $lock_path is a symlink — refusing to open (symlink attack surface)",
    )
    flags = ipc_owner_lock_open_flags()
    fd = ccall(:open, Cint, (Cstring, Cint, Cint), lock_path, flags, 0o600)
    if fd < 0
        error("Cannot open IPC owner lock $lock_path")
    end
    # Non-Linux: set FD_CLOEXEC after open (best-effort).
    if !Sys.islinux()
        try
            # F_GETFD=1, F_SETFD=2, FD_CLOEXEC=1
            cur = ccall(:fcntl, Cint, (Cint, Cint), fd, Cint(1))
            if cur >= 0
                ccall(:fcntl, Cint, (Cint, Cint, Cint), fd, Cint(2), cur | Cint(1))
            end
        catch
        end
    end
    # LOCK_EX=2, LOCK_NB=4
    if ccall(:flock, Cint, (Cint, Cint), fd, Cint(6)) != 0
        ccall(:close, Cint, (Cint,), fd)
        error(
            "IPC endpoint already locked by another live broadcaster ($lock_path). " *
            "Stop that process first; do not rely on unlinking its socket.",
        )
    end
    # Best-effort PID note for operators (not used for ownership).
    try
        ccall(:ftruncate, Cint, (Cint, Clong), fd, 0)
        pid_bytes = Vector{UInt8}(string(getpid()))
        ccall(:write, Cssize_t, (Cint, Ptr{UInt8}, Csize_t), fd, pid_bytes, length(pid_bytes))
    catch
    end
    return lock_path, fd
end

"""
Release exclusive flock and close the fd. Does **not** unlink `*.owner.lock`
(see acquire docstring — stable inode is required for mutual exclusion).
"""
function release_ipc_owner_lock(
    lock_path::Union{Nothing,AbstractString},
    fd::Cint,
)
    # lock_path retained for API stability / logging; file stays on disk.
    if fd >= 0
        try
            ccall(:flock, Cint, (Cint, Cint), fd, Cint(8))  # LOCK_UN
        catch
        end
        try
            ccall(:close, Cint, (Cint,), fd)
        catch
        end
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
        release_ipc_owner_lock(broadcaster.owner_lock_path, broadcaster.owner_lock_fd)
        broadcaster.owner_lock_path = nothing
        broadcaster.owner_lock_fd = Cint(-1)
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
