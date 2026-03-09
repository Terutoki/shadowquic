#!/bin/bash

# ShadowQUIC Extreme Performance Tuning Script
# Run as root

set -e

echo "=== ShadowQUIC Extreme Performance Tuning ==="
echo ""

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "Please run as root"
    exit 1
fi

# Get network interface
IFACE=${1:-eth0}

echo "[1/8] Applying kernel network tuning..."
# Network kernel parameters
sysctl -w net.core.rmem_max=134217728
sysctl -w net.core.wmem_max=134217728
sysctl -w net.core.rmem_default=16777216
sysctl -w net.core.wmem_default=16777216
sysctl -w net.core.netdev_max_backlog=250000
sysctl -w net.core.somaxconn=65535
sysctl -w net.core.optmem_max=25165824

# TCP tuning
sysctl -w net.ipv4.tcp_rmem="16777216 16777216 134217728"
sysctl -w net.ipv4.tcp_wmem="16777216 16777216 134217728"
sysctl -w net.ipv4.udp_rmem_min=16777216
sysctl -w net.ipv4.udp_wmem_min=16777216
sysctl -w net.ipv4.tcp_fastopen=3
sysctl -w net.ipv4.tcp_tw_reuse=1
sysctl -w net.ipv4.tcp_max_syn_backlog=65535
sysctl -w net.ipv4.tcp_syncookies=0
sysctl -w net.ipv4.tcp_notsent_lowat=16384

# Enable BBR
sysctl -w net.ipv4.tcp_congestion_control=bbr
sysctl -w net.core.default_qdisc=fq

echo "[2/8] Disabling TCP coalescing for $IFACE..."
ethtool -C $IFACE rx-usecs 0 tx-usecs 0 2>/dev/null || echo "ethtool not available, skipping"

echo "[3/8] Enabling RSS for $IFACE..."
ethtool -K $IFACE rx on tx on 2>/dev/null || echo "ethtool not available, skipping"

echo "[4/8] Getting queue count..."
QUEUE_COUNT=$(ls -1 /sys/class/net/$IFACE/queues/ 2>/dev/null | grep -c "^rx-" || echo "0")
CPU_COUNT=$(nproc)

echo "  Detected $QUEUE_COUNT queues, $CPU_COUNT CPUs"

echo "[5/8] Configuring RPS (Receive Packet Steering)..."
for queue in /sys/class/net/$IFACE/queues/rx-*/; do
    queue_num=$(basename "$queue" | sed 's/rx-//')
    cpu=$((queue_num % CPU_COUNT))
    mask=$(printf '%x' $((1 << cpu)))
    echo "$mask" > "${queue}rps_cpus" 2>/dev/null || true
done

echo "[6/8] Configuring XPS (Transmit Packet Steering)..."
for queue in /sys/class/net/$IFACE/queues/tx-*/; do
    queue_num=$(basename "$queue" | sed 's/tx-//')
    cpu=$((queue_num % CPU_COUNT))
    mask=$(printf '%x' $((1 << cpu)))
    echo "$mask" > "${queue}xps_cpus" 2>/dev/null || true
done

echo "[7/8] Disabling IRQ balancing..."
service irqbalance stop 2>/dev/null || true

echo "[8/8] Setting IRQ affinity..."
# Try to set IRQ affinity for network IRQs
for irq in /proc/irq/*/; do
    irq_num=$(basename "$irq")
    if [ -f "$irq/affinity_hint" ]; then
        # Simple affinity - just use CPU 0 for simplicity
        echo 1 > "$irq/smp_affinity" 2>/dev/null || true
    fi
done

echo ""
echo "=== Tuning Complete ==="
echo ""
echo "To persist these settings, add to /etc/sysctl.conf:"
echo ""
cat << 'EOF'
net.core.rmem_max=134217728
net.core.wmem_max=134217728
net.core.netdev_max_backlog=250000
net.core.somaxconn=65535
net.ipv4.tcp_rmem=16777216 16777216 134217728
net.ipv4.tcp_wmem=16777216 16777216 134217728
net.ipv4.udp_rmem_min=16777216
net.ipv4.udp_wmem_min=16777216
net.ipv4.tcp_fastopen=3
net.ipv4.tcp_tw_reuse=1
net.ipv4.tcp_max_syn_backlog=65535
net.ipv4.tcp_congestion_control=bbr
net.core.default_qdisc=fq
EOF
