#!/bin/bash
# End-to-End Integration Checks for Limen-Capital (experimental SNN-HFT)
# Structure + dependency presence; does not require full GPU brain init.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIMEN_NEURAL="${LIMEN_NEURAL:-$(cd "$ROOT/../Limen-Neural" 2>/dev/null && pwd || echo "")}"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Limen-Capital — Integration Structure Check                 ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""
echo "ROOT=$ROOT"
echo "LIMEN_NEURAL=${LIMEN_NEURAL:-<not found>}"
echo ""

GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

TESTS_PASSED=0
TESTS_FAILED=0

pass() {
    echo -e "${GREEN}✓${NC} $1"
    TESTS_PASSED=$((TESTS_PASSED + 1))
}

fail() {
    echo -e "${RED}✗${NC} $1"
    TESTS_FAILED=$((TESTS_FAILED + 1))
}

warn() {
    echo -e "${YELLOW}⚠${NC} $1"
}

# Test 1: Directory structure
echo "Test 1: Directory structure..."
for d in execution brain math strategy proto; do
    if [ -d "$ROOT/$d" ]; then
        pass "$d/ exists"
    else
        fail "$d/ missing"
    fi
done

# Test 2: Limen-Neural quality deps — require git URL + rev pins (sibling optional)
echo ""
echo "Test 2: Limen-Neural dependencies..."
# Portable hex rev match (mawk lacks {7,} interval quantifiers).
_HEX7='[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]*'
assert_cargo_git_rev() {
    local crate="$1" url_substr="$2"
    # Require line-anchored crate = { git = "…url…", rev = "hex…" }.
    # URL must sit inside the quoted git value (not a comment coincidence).
    if awk -v crate="$crate" -v url="$url_substr" -v hex7="$_HEX7" '
        BEGIN { ok = 0 }
        $0 ~ "^"crate"[[:space:]]*=" {
            block = $0
            while (block !~ /}/ && (getline line) > 0) {
                block = block " " line
            }
            if (block ~ ("git[[:space:]]*=[[:space:]]*\"[^\"]*" url "[^\"]*\"") &&
                block ~ ("rev[[:space:]]*=[[:space:]]*\"" hex7 "\"")) {
                ok = 1
            }
        }
        END { exit(ok ? 0 : 1) }
    ' "$ROOT/execution/Cargo.toml"; then
        pass "$crate git+rev pin present"
    else
        fail "$crate missing git URL and/or rev pin in execution/Cargo.toml"
    fi
}
assert_julia_source_rev() {
    local pkg="$1" url_substr="$2"
    # Only accept [sources] rows with url="…expected…" and rev="hex…".
    if awk -v pkg="$pkg" -v url="$url_substr" -v hex7="$_HEX7" '
        BEGIN { ok = 0; in_sources = 0 }
        /^\[/ {
            in_sources = ($0 ~ /^\[sources\]/)
            next
        }
        in_sources && $0 ~ "^"pkg"[[:space:]]*=" {
            if ($0 ~ ("url[[:space:]]*=[[:space:]]*\"[^\"]*" url "[^\"]*\"") &&
                $0 ~ ("rev[[:space:]]*=[[:space:]]*\"" hex7 "\"")) {
                ok = 1
            }
        }
        END { exit(ok ? 0 : 1) }
    ' "$ROOT/brain/Project.toml"; then
        pass "Julia $pkg [sources] url+rev pin present"
    else
        fail "Julia $pkg missing url and/or rev in brain/Project.toml [sources]"
    fi
}
assert_cargo_git_rev "metabolic-ledger" "github.com/Limen-Neural/metabolic-ledger"
assert_cargo_git_rev "kinetic-signals" "github.com/Limen-Neural/kinetic-signals"
assert_cargo_git_rev "neuromod" "github.com/Limen-Neural/neuromod"
assert_julia_source_rev "LiquidCortex" "github.com/Limen-Neural/LiquidCortex.jl"
assert_julia_source_rev "TemporalFocus" "github.com/Limen-Neural/NeuroPulse.jl"
if [ -n "$LIMEN_NEURAL" ] && [ -d "$LIMEN_NEURAL" ]; then
    pass "Optional Limen-Neural sibling found at $LIMEN_NEURAL"
    for lib in metabolic-ledger LiquidCortex.jl NeuroPulse.jl kinetic-signals; do
        if [ -d "$LIMEN_NEURAL/$lib" ]; then
            pass "sibling $lib present"
        else
            warn "sibling $lib missing under $LIMEN_NEURAL (ok if using git pins only)"
        fi
    done
else
    warn "No Limen-Neural sibling (set LIMEN_NEURAL=... only if developing against local clones)"
fi

# Test 3: Cargo.toml uses metabolic-ledger
echo ""
echo "Test 3: Cargo.toml Limen-Neural wiring..."
if grep -q 'metabolic-ledger' "$ROOT/execution/Cargo.toml"; then
    pass "metabolic-ledger dependency configured"
else
    fail "metabolic-ledger dependency missing"
fi
if grep -q 'spikenaut-ghost\|ballast-lab\|soma-engine\|spikenaut-spine\|spikenaut-ingest' "$ROOT/execution/Cargo.toml"; then
    fail "legacy spikenaut path deps still present"
else
    pass "legacy spikenaut path deps removed"
fi

# Test 4: Rust compilation
echo ""
echo "Test 4: Rust compilation (cargo check)..."
if (cd "$ROOT/execution" && cargo check --locked --quiet 2>/tmp/limen_cargo_check.err); then
    pass "Rust execution engine compiles"
else
    fail "Rust compilation failed (see /tmp/limen_cargo_check.err)"
    head -40 /tmp/limen_cargo_check.err || true
fi

# Test 5: Training binary
echo ""
echo "Test 5: Training binary..."
if [ -f "$ROOT/execution/src/bin/train_ghost.rs" ]; then
    pass "train_ghost.rs exists"
else
    fail "train_ghost.rs missing"
fi

# Test 6: Julia Project.toml
echo ""
echo "Test 6: Julia brain Project.toml..."
if [ -f "$ROOT/brain/Project.toml" ]; then
    pass "brain/Project.toml exists"
    grep -q 'CUDA' "$ROOT/brain/Project.toml" && pass "CUDA.jl configured" || fail "CUDA.jl missing"
    grep -q 'ZMQ' "$ROOT/brain/Project.toml" && pass "ZMQ.jl configured" || fail "ZMQ.jl missing"
else
    fail "brain/Project.toml missing"
fi

# Test 7: Core Julia files
echo ""
echo "Test 7: Core Julia brain files..."
for file in naut_core.jl spike_helm.jl synapse_conductor.jl market_encoder.jl; do
    if [ -f "$ROOT/brain/$file" ]; then
        pass "$file exists"
    else
        fail "$file missing"
    fi
done

# Test 8: GPU (optional)
echo ""
echo "Test 8: GPU (optional)..."
if command -v nvidia-smi &>/dev/null; then
    VRAM_MB=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits | head -1)
    VRAM_GB=$((VRAM_MB / 1024))
    if [ "$VRAM_GB" -ge 16 ]; then
        pass "VRAM ${VRAM_GB} GB (full-N research OK)"
    else
        warn "VRAM ${VRAM_GB} GB (use small-N / MC off)"
    fi
else
    warn "nvidia-smi not available"
fi

# Test 9: Docs
echo ""
echo "Test 9: Documentation..."
[ -f "$ROOT/README.md" ] && pass "README.md" || fail "README.md missing"
[ -f "$ROOT/docs/deps.md" ] && pass "docs/deps.md" || fail "docs/deps.md missing"
[ -f "$ROOT/docs/wire-protocol-v1.md" ] && pass "docs/wire-protocol-v1.md" || fail "docs/wire-protocol-v1.md missing"

# Test 10: No hardcoded fake CURVE secrets in spike_helm
echo ""
echo "Test 10: Secret hygiene..."
if grep -q 'R)@\[+f1J' "$ROOT/brain/spike_helm.jl" 2>/dev/null; then
    fail "Hardcoded fake CURVE key still in spike_helm.jl"
else
    pass "No hardcoded CURVE key material in spike_helm.jl"
fi

# Summary
echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║                      Test Summary                            ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo -e "Passed: ${GREEN}${TESTS_PASSED}${NC}  Failed: ${RED}${TESTS_FAILED}${NC}"
echo ""

if [ "$TESTS_FAILED" -eq 0 ]; then
    echo -e "${GREEN}✓ Structure checks passed${NC}"
    echo ""
    echo "Next steps:"
    echo "  1. cargo test -p spikenaut-execution-engine  (cd execution)"
    echo "  2. julia --project=brain test/runtests.jl"
    echo "  3. research: julia --project=brain research_helm.jl"
    exit 0
else
    echo -e "${RED}✗ Some checks failed${NC}"
    exit 1
fi
