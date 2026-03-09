#!/bin/bash

# ShadowQUIC CPU Affinity Pinning Script
# Pins application threads to specific CPU cores for optimal performance

set -e

# Default values
PID=${1:-$(pgrep -f shadowquic | head -1)}
CPUS=${2:-"0-7"}

if [ -z "$PID" ]; then
    echo "Usage: $0 <pid> <cpu_range>"
    echo "Example: $0 12345 0-7"
    echo ""
    echo "Find your process: pgrep -f shadowquic"
    exit 1
fi

echo "Pinning ShadowQUIC (PID: $PID) to CPUs: $CPUS"

# Get thread count
THREAD_COUNT=$(ps -Tp $PID | wc -l)
echo "Process has $THREAD_COUNT threads"

# Get CPU list
CPU_LIST=$(seq -s, 0 7)
echo "Available CPU list: $CPU_LIST"

# Get all threads of the process
TIDS=$(ps -Tp $PID -o tid= | tr '\n' ' ')

# Pin main process
echo "Pinning main thread..."
taskset -cp $CPUS $PID

# Pin each thread (if supported)
echo "Pinning worker threads..."
for TID in $TIDS; do
    # Use rotation to distribute across cores
    CPU_IDX=$((TID % 8))
    taskset -cp $CPU_IDX -p $TID 2>/dev/null || true
done

echo ""
echo "Pinning complete!"
echo ""
echo "Verify with: ps -o pid,tid,psr,comm -p $PID"
echo ""
echo "To make persistent, use systemd:"
echo "[Service]"
echo "CPUSchedulingPolicy=fifo"
echo "CPUSchedulingPriority=99"
echo "CPUAffinity=0-7"
