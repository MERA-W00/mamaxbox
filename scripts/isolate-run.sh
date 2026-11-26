#!/usr/bin/env bash
# ==============================================================================
# VIctory / Isopod - Kernel-Level Network Isolation Runner
# ==============================================================================
# Confines any command (e.g., Proton, victory.exe, or GTA VI Scarlett runtime)
# within an unprivileged Linux network namespace (CLONE_NEWNET + CLONE_NEWUSER).
#
# Guarantees:
# 1. Zero access to physical or wireless interfaces (wlan0, eth0, enp*, etc.).
# 2. No default route, no gateway, no external IP reachability.
# 3. Dedicated loopback ('lo') interface brought UP so local IPC
#    (such as the DualSense PS5 controller bridge on 127.0.0.1:28777) functions.
# 4. Zero root / sudo requirements (uses user namespaces).
# ==============================================================================

set -euo pipefail

audit_environment() {
    echo -e "\033[1;36m=== Linux Kernel Network Isolation Audit ===\033[0m"
    local interfaces
    interfaces=$(grep -v "lo:" /proc/net/dev 2>/dev/null | tail -n +3 | awk -F: '{print $1}' | tr -d ' ' || true)
    local route_count
    route_count=$(tail -n +2 /proc/net/route 2>/dev/null | wc -l || echo 0)

    echo -n "• External network interfaces: "
    if [ -z "$interfaces" ]; then
        echo -e "\033[1;32mNone (Loopback only - Isolated)\033[0m"
    else
        echo -e "\033[1;33mDetected: $interfaces\033[0m"
    fi

    echo -n "• Kernel routing table entries: "
    if [ "$route_count" -eq 0 ]; then
        echo -e "\033[1;32m0 (No gateway/routes - Isolated)\033[0m"
    else
        echo -e "\033[1;33m$route_count active routes\033[0m"
    fi

    echo -n "• Outbound test connection (198.51.100.1): "
    if python3 -c 'import socket; s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.connect(("198.51.100.1", 80))' 2>/dev/null; then
        echo -e "\033[1;31mREACHABLE (Not isolated)\033[0m"
    else
        echo -e "\033[1;32mDROPPED / ENETUNREACH (Kernel blocked)\033[0m"
    fi

    echo -n "• Local loopback (127.0.0.1): "
    if python3 -c 'import socket; s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(("127.0.0.1", 0))' 2>/dev/null; then
        echo -e "\033[1;32mACTIVE (Local IPC ready)\033[0m"
    else
        echo -e "\033[1;31mINACTIVE\033[0m"
    fi

    echo -e "\033[1;36m============================================\033[0m"
}

if [ "${1:-}" = "--check" ] || [ "${1:-}" = "--status" ]; then
    audit_environment
    exit 0
fi

if [ $# -eq 0 ]; then
    echo "Usage: $0 [--check | <command> [args...]]"
    echo "Runs <command> inside a private, kernel-isolated network namespace."
    exit 1
fi

# Check if we are already inside an isolated netns (no external interfaces and 0 routes)
non_lo_ifaces=$(grep -v "lo:" /proc/net/dev 2>/dev/null | tail -n +3 | awk -F: '{print $1}' | tr -d ' ' || true)
route_cnt=$(tail -n +2 /proc/net/route 2>/dev/null | wc -l || echo 0)

if [ -z "$non_lo_ifaces" ] && [ "$route_cnt" -eq 0 ]; then
    # Already isolated; execute directly
    exec "$@"
fi

# Launch inside unshare network namespace with user namespace mapping
if command -v unshare >/dev/null 2>&1; then
    exec unshare --net --map-root-user sh -c '
        ip link set lo up 2>/dev/null || true
        exec "$@"
    ' -- "$@"
elif command -v bwrap >/dev/null 2>&1; then
    exec bwrap --unshare-net --dev-bind / / -- sh -c '
        ip link set lo up 2>/dev/null || true
        exec "$@"
    ' -- "$@"
else
    echo "Error: Neither unshare nor bwrap is available on this system." >&2
    exit 1
fi
