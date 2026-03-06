#!/bin/bash
# Profile-Guided Optimization (PGO) script for ShadowQUIC
# This script performs PGO to optimize the binary based on runtime profiling

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="$PROJECT_ROOT/target/release"
PROF_DATA="$PROJECT_ROOT/target/profdata"

echo "=== ShadowQUIC PGO Build Script ==="
echo "Project root: $PROJECT_ROOT"

# Step 1: Build instrumented binary
echo ""
echo "[1/4] Building instrumented binary..."
RUSTFLAGS="-C target-cpu=native -C profile-generate=$PROF_DATA" \
    cargo build --release

# Step 2: Run profiling workload
echo ""
echo "[2/4] Running profiling workload..."
echo "Please run your typical workload in another terminal."
echo "Press Enter when done..."
read

# Alternative: Automated profiling with iperf3
# echo "Running automated profiling..."
# ./target/release/shadowquic &
# SERVER_PID=$!
# sleep 2
# iperf3 -c localhost -t 30
# kill $SERVER_PID

# Step 3: Merge profile data
echo ""
echo "[3/4] Merging profile data..."
PROFRAW_FILES=$(find $PROF_DATA -name "*.profraw")
if [ -z "$PROFRAW_FILES" ]; then
    echo "Error: No profile data found. Did you run the workload?"
    exit 1
fi

llvm-profdata merge -o $PROF_DATA/merged.profdata $PROF_DATA/*.profraw

# Step 4: Build optimized binary
echo ""
echo "[4/4] Building optimized binary with PGO..."
RUSTFLAGS="-C target-cpu=native -C profile-use=$PROF_DATA/merged.profdata" \
    cargo build --release

echo ""
echo "=== PGO build complete ==="
echo "Optimized binary: $BUILD_DIR/shadowquic"
echo "Profile data: $PROF_DATA/merged.profdata"
