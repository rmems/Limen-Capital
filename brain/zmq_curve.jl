# zmq_curve.jl — optional CURVE + ZAP client allowlist for brain ZMQ sockets.
#
# Default path is plain TCP loopback (ZMQ_CURVE unset). See docs/SECURITY.md.

module ZmqCurve

using ZMQ

const DEFAULT_ZAP_DOMAIN = "limen-capital"

curve_enabled() = get(ENV, "ZMQ_CURVE", "0") == "1"

function zap_domain()
    d = strip(get(ENV, "ZMQ_ZAP_DOMAIN", DEFAULT_ZAP_DOMAIN))
    isempty(d) ? DEFAULT_ZAP_DOMAIN : d
end

function _trim_key(line::AbstractString)
    s = strip(line)
    isempty(s) || startswith(s, "#") ? nothing : s
end

"""Parse `ZMQ_CURVE_ALLOWLIST` / `ZMQ_CURVE_ALLOWLIST_FILE`. `nothing` = no ZAP."""
function load_curve_allowlist()
    keys = String[]
    if haskey(ENV, "ZMQ_CURVE_ALLOWLIST")
        for part in split(ENV["ZMQ_CURVE_ALLOWLIST"], [',', ';', ' ', '\t', '\n']; keepempty=false)
            k = _trim_key(part)
            k !== nothing && push!(keys, k)
        end
    end
    if haskey(ENV, "ZMQ_CURVE_ALLOWLIST_FILE")
        path = strip(ENV["ZMQ_CURVE_ALLOWLIST_FILE"])
        isempty(path) && error("ZMQ_CURVE_ALLOWLIST_FILE is empty")
        open(path) do io
            for line in eachline(io)
                k = _trim_key(line)
                k !== nothing && push!(keys, k)
            end
        end
    end
    unique!(keys)
    isempty(keys) ? nothing : keys
end

function _first_env(names::AbstractVector{String})
    for n in names
        haskey(ENV, n) || continue
        v = strip(ENV[n])
        !isempty(v) && return v
    end
    return nothing
end

market_server_secret() = _first_env(["ZMQ_MARKET_SERVER_SECRET", "ZMQ_SERVER_KEY"])
readout_server_secret() = _first_env(["ZMQ_READOUT_SERVER_SECRET", "ZMQ_SERVER_KEY"])
client_secret() = _first_env(["ZMQ_CLIENT_SECRET_KEY", "ZMQ_CURVE_SECRET_KEY", "ZMQ_SERVER_KEY"])
market_server_public() = _first_env(["ZMQ_MARKET_SERVER_PUBLIC_KEY", "ZMQ_MARKET_PUB_KEY"])
readout_server_public() = _first_env(["ZMQ_READOUT_SERVER_PUBLIC_KEY"])  # legacy below for muscle only

function apply_curve_client!(sock::ZMQ.Socket, server_public_z85::String, client_secret_z85::String)
    client_pub = get(ENV, "ZMQ_CLIENT_PUBLIC_KEY", nothing)
    client_pub = client_pub === nothing ? nothing : strip(client_pub)
    if client_pub === nothing || isempty(client_pub)
        # libzmq derives public key from secret (Z85)
        buf = Vector{UInt8}(undef, 41)
        GC.@preserve client_secret_z85 buf begin
            rc = ccall((:zmq_curve_public, ZMQ.libzmq),
                Cint,
                (Ptr{Cchar}, Ptr{Cchar}),
                buf, client_secret_z85)
            rc != 0 && error("zmq_curve_public failed")
        end
        nul = findfirst(==(0), buf)
        client_pub = String(buf[1:(nul === nothing ? 40 : nul - 1)])
    end
    ZMQ.set_curve_server(sock, false)
    ZMQ.set_curve_serverkey(sock, server_public_z85)
    ZMQ.set_curve_publickey(sock, client_pub)
    ZMQ.set_curve_secretkey(sock, client_secret_z85)
    return sock
end

function apply_curve_server!(sock::ZMQ.Socket, server_secret_z85::String; zap::Bool=false)
    ZMQ.set_curve_server(sock, true)
    ZMQ.set_curve_secretkey(sock, server_secret_z85)
    if zap
        ZMQ.set_zap_domain(sock, zap_domain())
    end
    return sock
end

mutable struct ZapAllowlistState
    stop::Threads.Atomic{Bool}
    task::Union{Task,Nothing}
end

ZapAllowlistState() = ZapAllowlistState(Threads.Atomic{Bool}(false), nothing)

function stop_zap!(state::ZapAllowlistState)
    state.stop[] = true
    state.task !== nothing && wait(state.task)
    state.task = nothing
end

"""Start in-process ZAP handler on `ctx` (same context as server sockets)."""
function start_zap_allowlist!(ctx::ZMQ.Context, allowlist::Vector{String})
    allowed = Set(allowlist)
    state = ZapAllowlistState()
    state.task = @async begin
        sock = ZMQ.Socket(ctx, ZMQ.REP)
        try
            ZMQ.bind(sock, "inproc://zeromq.org")
            while !state.stop[]
                parts = ZMQ.recv_multipart(sock)
                length(parts) < 7 && continue
                version = String(parts[1])
                seq = parts[2]
                mech = String(parts[6])
                user = String(parts[7])
                status, text = if version != "1.0"
                    (400, "Invalid version")
                elseif mech != "CURVE"
                    (400, "Invalid mechanism")
                elseif user in allowed
                    (200, "OK")
                else
                    (400, "Unauthorized CURVE client")
                end
                reply = [b"1.0", seq, string(status), text, UInt8[], UInt8[]]
                ZMQ.send_multipart(sock, reply)
            end
        finally
            close(sock)
        end
    end
    return state
end

export curve_enabled, load_curve_allowlist, apply_curve_client!, apply_curve_server!
export start_zap_allowlist!, stop_zap!, ZapAllowlistState, zap_domain
export market_server_secret, readout_server_secret, client_secret, market_server_public

end # module
