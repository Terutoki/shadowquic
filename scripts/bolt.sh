#!/bin/bash
# BOLT (Binary Optimization and Layout Tool) script for ShadowQUIC
# This script performs BOLT optimization on the binary

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="$PROJECT_ROOT/target/release"
BOLT_DIR="$PROJECT_ROOT/target/bolt"

echo "=== ShadowQUIC BOLT Optimization Script ==="
echo "Project root: $PROJECT_ROOT"

# Check for BOLT installation
if ! command -v llvm-bolt &> /dev/null; then
    echo "Error: llvm-bolt not found. Please install BOLT first."
    echo "On Ubuntu/Debian: sudo apt install llvm-bolt"
    echo "On macOS: brew install llvm-bolt"
    exit 1
fi

# Step 1: Build release binary
echo ""
echo "[1/6] Building release binary..."
RUSTFLAGS="-C target-cpu=native" cargo build --release

# Step 2: Prepare BOLT directory
echo ""
echo "[2/6] Preparing BOLT directory..."
mkdir -p $BOLT_DIR
cp $BUILD_DIR/shadowquic $BOLT_DIR/shadowquic.bolt

# Step 3: Run profiling with perf
echo ""
echo "[3/6] Running profiling..."
echo "Please run your typical workload in another terminal."
echo "Press Enter when done..."
read

# Alternative: Automated profiling
# perf record -e cycles:u -j any,u -o $BOLT_DIR/perf.data -- ./target/release/shadowquic &

# Step 4: Convert perf data to BOLT format
echo ""
echo "[4/6] Converting perf data..."
if [ -f "$BOLT_DIR/perf.data" ]; then
    perf2bolt -p $BOLT_DIR/perf.data -o $BOLT_DIR/shadowquic.fdata $BOLT_DIR/shadowquic.bolt
else
    echo "Warning: No perf.data found. Skipping BOLT optimization."
    exit 0
fi

# Step 5: Run BOLT optimization
echo ""
echo "[5/6] Running BOLT optimization..."
llvm-bolt $BOLT_DIR/shadowquic.bolt \
    -o $BUILD_DIR/shadowquic.bolt \
    -data $BOLT_DIR/shadowquic.fdata \
    -reorder-blocks=cache+ \
    -reorder-functions=hfsort+ \
    -split-functions=3 \
    -split-all-cold \
    -dyno-stats \
    -icf=1 \
    -use-gnu-stack

# Step 6: Replace original binary
echo ""
echo "[6/6] Replacing binary..."
mv $BUILD_DIR/shadowquic $BUILD_DIR/shadowquic.original
mv $BUILD_DIR/shadowquic.bolt $BUILD_DIR/shadowquic
chmod +x $BUILD_DIR/shadowquic

echo ""
echo "=== BOLT optimization complete ==="
echo "Original binary: $BUILD_DIR/shadowquic.original"
echo "Optimized binary: $BUILD_DIR/shadowquic"
