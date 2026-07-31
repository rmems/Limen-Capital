# spike_recorder.jl — SNN High-Frequency Spike Event Recorder
#
# Part of the Spikenaut SNN HFT Research Partition.
# Records (tick, neuron_id) events for 4 lobes (262,144 neurons).
# Binary format: [tick (UInt64), neuron_id (UInt32)] (12 bytes/spike)
# Data volume: ~12 MB per 1M spikes. Safe for RTX 5080 level stimulation.

mutable struct SpikeRecorder
    file_path::String
    io::IOStream
    buffer::Vector{UInt8}
    buf_ptr::Int
    max_buf::Int
    total_spikes::Int64
end

"""
    SpikeRecorder(path="spikes_raw.bin", buf_size=1048576)
Initialize a binary recorder with a 1MB write buffer.
"""
function SpikeRecorder(path::String="spikes_raw.bin"; buf_size::Int=1048576)
    io = open(path, "w")
    println("[recorder] Recording spikes to $path (Binary: 12 bytes/event)")
    SpikeRecorder(path, io, zeros(UInt8, buf_size), 1, buf_size, 0)
end

"""
    record_spikes!(rec, tick, spikes_gpu)
Extracts neuron indices from a binary spike vector (0/1) and records them.
"""
function record_spikes!(rec::SpikeRecorder, tick::Int64, spikes_gpu::CuVector{Float32})
    # Find indices of spiked neurons (S > 0.5)
    # Note: For maximum performance, this can be done on the GPU with findall
    # But for research, we'll extract to host for standard binary writing.
    spiked_indices = Array(findall(spikes_gpu .> 0.5f0))
    
    for idx in spiked_indices
        # Check if buffer has space (12 bytes required)
        if rec.buf_ptr + 12 > rec.max_buf
            flush_recorder!(rec)
        end
        
        # Pack UInt64 tick (bytes 1-8)
        # Pack UInt32 neuron_id (bytes 9-12)
        # Note: idx is 1-based in Julia, neuron_id is 0-based for neuromorphic compatibility
        u_tick = reinterpret(UInt8, [UInt64(tick)])
        u_id = reinterpret(UInt8, [UInt32(idx - 1)])
        
        copyto!(rec.buffer, rec.buf_ptr, u_tick, 1, 8)
        copyto!(rec.buffer, rec.buf_ptr + 8, u_id, 1, 4)
        
        rec.buf_ptr += 12
        rec.total_spikes += 1
    end
    
    return nothing
end

"""
    flush_recorder!(rec)
Writes the internal buffer to disk.
"""
function flush_recorder!(rec::SpikeRecorder)
    if rec.buf_ptr > 1
        write(rec.io, @view rec.buffer[1:rec.buf_ptr-1])
        rec.buf_ptr = 1
    end
    return nothing
end

"""
    close_recorder!(rec)
Flushes remaining data and closes the file.
"""
function close_recorder!(rec::SpikeRecorder)
    flush_recorder!(rec)
    close(rec.io)
    println("[recorder] Closed $rec.file_path. Total spikes recorded: $rec.total_spikes")
end

println("[recorder] spike_recorder.jl loaded — Ready for Neuromorphic Data Generation")
