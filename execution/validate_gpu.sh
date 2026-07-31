#!/bin/bash
# GPU Validation Script for Single-GPU Configuration
# Validates RTX 5080 (16GB VRAM) constraints for Spikenaut Ghost Trader

echo "=== Spikenaut Ghost Trader GPU Validation ==="
echo ""

# Check NVIDIA GPU presence
echo "1. Checking NVIDIA GPU..."
if command -v nvidia-smi &> /dev/null; then
    nvidia-smi --query-gpu=name,memory.total --format=csv,noheader
    echo "✓ NVIDIA GPU detected"
else
    echo "✗ nvidia-smi not found"
    exit 1
fi

echo ""
echo "2. Checking CUDA availability..."
if command -v nvcc &> /dev/null; then
    nvcc --version | grep "release"
    echo "✓ CUDA toolkit installed"
else
    echo "✗ CUDA toolkit not found"
fi

echo ""
echo "3. Validating single-GPU configuration..."
GPU_COUNT=$(nvidia-smi --query-gpu=count --format=csv,noheader | head -1)
echo "GPU count: $GPU_COUNT"

if [ "$GPU_COUNT" -eq 1 ]; then
    echo "✓ Single-GPU configuration confirmed"
else
    echo "⚠ Multiple GPUs detected. Ensure CUDA_VISIBLE_DEVICES=0 for single-GPU operation"
fi

echo ""
echo "4. Checking VRAM capacity..."
VRAM_MB=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits | head -1)
VRAM_GB=$((VRAM_MB / 1024))
echo "Total VRAM: ${VRAM_GB} GB"

if [ "$VRAM_GB" -ge 16 ]; then
    echo "✓ Sufficient VRAM for Spikenaut (16GB required)"
else
    echo "✗ Insufficient VRAM. 16GB required, ${VRAM_GB}GB detected"
    exit 1
fi

echo ""
echo "5. Checking current VRAM usage..."
VRAM_USED=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | head -1)
VRAM_USED_GB=$((VRAM_USED / 1024))
VRAM_FREE=$((VRAM_MB - VRAM_USED))
VRAM_FREE_GB=$((VRAM_FREE / 1024))
echo "Used: ${VRAM_USED_GB} GB, Free: ${VRAM_FREE_GB} GB"

if [ "$VRAM_FREE_GB" -ge 12 ]; then
    echo "✓ Sufficient free VRAM for training (12-14GB required)"
else
    echo "⚠ Limited free VRAM. Consider stopping background processes"
fi

echo ""
echo "6. Checking Julia CUDA.jl availability..."
if command -v julia &> /dev/null; then
    julia -e 'using Pkg; Pkg.status("CUDA")' 2>&1 | grep -q "CUDA" && echo "✓ Julia CUDA.jl installed" || echo "✗ Julia CUDA.jl not found"
else
    echo "✗ Julia not found"
fi

echo ""
echo "=== GPU Validation Complete ==="
echo ""
echo "Configuration Summary:"
echo "  • Single-GPU: $([ "$GPU_COUNT" -eq 1 ] && echo 'Yes' || echo 'No')"
echo "  • VRAM Capacity: ${VRAM_GB} GB"
echo "  • VRAM Available: ${VRAM_FREE_GB} GB"
echo "  • CUDA Toolkit: $(command -v nvcc &> /dev/null && echo 'Installed' || echo 'Not Found')"
echo ""
echo "Recommended Settings:"
echo "  • Export CUDA_VISIBLE_DEVICES=0 (force single-GPU)"
echo "  • Stop background mining during training"
echo "  • Monitor VRAM with: watch -n 1 nvidia-smi"
