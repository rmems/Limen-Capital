#=
signal_broadcaster.jl — JSON adapter (secondary path)

Primary research wire is binary ReadoutPacket on LIMEN_IPC_PUB (see wire/README.md).
This module is the **JSON IPC adapter** for tools that still emit TradeSignal JSON
to ipc:///tmp/spikenaut_signals.ipc (LIMEN_WIRE=json on the Rust muscle).

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

"""
    SignalBroadcaster

Maintains ZMQ publisher socket for broadcasting trade signals to Rust execution engine.
Thread-safe wrapper around ZMQ context and socket.
"""
mutable struct SignalBroadcaster
    context::ZMQ.Context
    socket::ZMQ.Socket
    message_count::Int64

    function SignalBroadcaster()
        context = ZMQ.Context()
        socket = ZMQ.Socket(context, ZMQ.PUB)
        ZMQ.bind(socket, "ipc:///tmp/spikenaut_signals.ipc")

        println("[$(now())] SignalBroadcaster initialized at ipc:///tmp/spikenaut_signals.ipc")
        new(context, socket, 0)
    end
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
export SignalBroadcaster, broadcast_trade, shutdown, test_broadcast
