# hft_inhibition.jl — Single stress → inhibition map (no CUDA, no packages)
#
# Shared by naut_core step! and LiquidCortex path (via reservoir.jl).
# Output scalar is in [0, 3], matching LiquidCortex MAX_INHIBITION scale.

"""
    hft_inhibition(gpu_temp, basys_load, funding_rate, liquidation_vol;
                   dydx_oi_delta=0, dydx_funding_rate=0) -> Float32

Map hardware + market stress into a single inhibition scalar for V_thresh raise.
"""
function hft_inhibition(
    gpu_temp::Float32, basys_load::Float32,
    funding_rate::Float32, liquidation_vol::Float32;
    dydx_oi_delta::Float32=0.0f0,
    dydx_funding_rate::Float32=0.0f0,
)::Float32
    TEMP_THRESH = 75.0f0
    BUFFER_THRESH = 0.8f0
    inhib = 0.0f0
    if gpu_temp > TEMP_THRESH
        inhib += (gpu_temp - TEMP_THRESH) / 25.0f0
    end
    if basys_load > BUFFER_THRESH
        inhib += (basys_load - BUFFER_THRESH) / 0.2f0
    end
    if abs(funding_rate) > 0.0005f0
        inhib += clamp(abs(funding_rate) / 0.001f0, 0.0f0, 1.5f0)
    end
    if liquidation_vol > 10_000_000.0f0
        inhib += clamp(liquidation_vol / 50_000_000.0f0, 0.0f0, 1.0f0)
    end
    if abs(dydx_funding_rate) > 0.0003f0
        inhib += clamp(abs(dydx_funding_rate) / 0.001f0, 0.0f0, 1.0f0)
    end
    oi_arousal = clamp(dydx_oi_delta / 0.05f0, -1.0f0, 1.0f0)
    inhib -= 0.5f0 * oi_arousal
    return clamp(inhib, 0.0f0, 3.0f0)
end
