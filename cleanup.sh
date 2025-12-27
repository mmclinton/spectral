#!/usr/bin/env bash

# cleanup.sh - Clean shutdown and cleanup for fresh Spectral builds
# Ensures all processes, sockets, and audio nodes are removed between builds
#
# Usage:
#   ./cleanup.sh        - Standard cleanup for development builds
#   ./cleanup.sh --deep - Full cleanup including audio service restart

set -euo pipefail

DEEP_CLEAN=false
[[ "${1:-}" =~ ^(--deep|-d)$ ]] && DEEP_CLEAN=true

echo "════════════════════════════════════════════════════════════"
if [[ "$DEEP_CLEAN" = true ]]; then
    echo "  Spectral Build Cleanup (DEEP MODE)"
else
    echo "  Spectral Build Cleanup"
fi
echo "════════════════════════════════════════════════════════════"

# 1. Stop all Spectral processes
echo ""
echo "[1/4] Stopping processes..."
if pgrep -f "spectral-daemon" >/dev/null 2>&1; then
    echo "  → Sending SIGTERM to daemon (graceful shutdown)..."
    pkill -TERM -f "spectral-daemon" 2>/dev/null || true
    
    # Wait up to 5 seconds for graceful shutdown
    for i in {1..10}; do
        pgrep -f "spectral-daemon" >/dev/null 2>&1 || break
        sleep 0.5
    done
    
    # Force kill if still running
    if pgrep -f "spectral-daemon" >/dev/null 2>&1; then
        echo "  → Forcing kill..."
        pkill -KILL -f "spectral-daemon" 2>/dev/null || true
        sleep 0.5
    fi
    echo "  ✓ Daemon stopped"
else
    echo "  - No daemon running"
fi

# Kill GUI and audio pipeline processes
pkill -KILL -f "^spectral$" 2>/dev/null && echo "  ✓ GUI stopped" || true
pkill -KILL -f "pw-record.*Spectral" 2>/dev/null && echo "  ✓ pw-record killed" || true
pkill -KILL -f "pw-play.*Spectral" 2>/dev/null && echo "  ✓ pw-play killed" || true

# 2. Remove IPC sockets
echo ""
echo "[2/4] Removing IPC sockets..."
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
rm -f "$RUNTIME_DIR/spectral-daemon.sock" 2>/dev/null && echo "  ✓ Removed socket" || echo "  - No socket found"
rm -f "$RUNTIME_DIR/spectral-daemon.log" 2>/dev/null && echo "  ✓ Removed log" || true

# 3. Clean up audio nodes
echo ""
echo "[3/4] Cleaning audio nodes..."

# Remove PipeWire metadata entries for Spectral
if command -v pw-metadata &>/dev/null; then
    if pw-metadata | grep -qi "spectral"; then
        # Find the first non-Spectral audio source to set as default
        FIRST_SOURCE=$(wpctl status 2>/dev/null | grep -A 20 "Sources:" | grep -E "^\s+[0-9]+\." | grep -v "Spectral" | head -1 | awk '{print $2}' | tr -d '.*')
        if [[ -n "$FIRST_SOURCE" ]]; then
            wpctl set-default "$FIRST_SOURCE" 2>/dev/null && echo "  ✓ Reset default source to ID $FIRST_SOURCE" || true
        fi
        # Clear the configured default
        pw-metadata -d default.configured.audio.source 2>/dev/null && echo "  ✓ Cleared Spectral metadata" || true
    fi
fi

# Find and destroy Spectral PipeWire nodes
if command -v pw-cli &>/dev/null; then
    pw-cli ls Node 2>/dev/null | awk '
        /^[[:space:]]*id [0-9]+/ { 
            current_id = $2; 
            gsub(",", "", current_id); 
        }
        /node.name =|node.description =/ { 
            if ($0 ~ /Spectral/) print current_id;
        }
    ' | sort -u | while read -r id; do
        [[ -n "$id" ]] && pw-cli destroy "$id" 2>/dev/null && echo "  ✓ Destroyed node $id" || true
    done
fi

# Unload PulseAudio modules
if command -v pactl &>/dev/null; then
    pactl list modules short 2>/dev/null | grep -i spectral | awk '{print $1}' | while read -r id; do
        pactl unload-module "$id" 2>/dev/null && echo "  ✓ Unloaded PA module $id" || true
    done
fi

# 4. Deep clean if requested
if [[ "$DEEP_CLEAN" = true ]]; then
    echo ""
    echo "[4/4] Deep clean - restarting audio..."
    systemctl --user restart wireplumber pipewire-pulse 2>/dev/null && echo "  ✓ Audio restarted" || echo "  ⚠ Restart failed"
    sleep 2
else
    echo ""
    echo "[4/4] Skipping audio restart (use --deep for full reset)"
fi

echo ""
echo "════════════════════════════════════════════════════════════"
echo "  ✓ Cleanup complete - ready for build"
echo "════════════════════════════════════════════════════════════"
