#!/usr/bin/env bash
set -e

# Capture host display server variables so the compositor window opens on your host desktop
HOST_WAYLAND="${WAYLAND_DISPLAY:-wayland-1}"
HOST_X11="${DISPLAY:-:0}"
PENUMBRA_SOCKET="wayland-penumbra-0"
# Check if user explicitly requested external COSMIC components
USE_COSMIC=false
if [ "${1:-}" = "--cosmic" ] || [ "${PENUMBRA_USE_COSMIC:-0}" = "1" ]; then
    USE_COSMIC=true
fi

cargo build --workspace

# Clean up any stale sockets and lock files
rm -f "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET"* "/tmp/$PENUMBRA_SOCKET"* 2>/dev/null || true

# 1. Start Penumbra Compositor
COMPOSITOR_ARGS="--socket $PENUMBRA_SOCKET"
if [ "$USE_COSMIC" = true ]; then
    COMPOSITOR_ARGS="$COMPOSITOR_ARGS --use-cosmic"
fi

WAYLAND_DISPLAY="$HOST_WAYLAND" DISPLAY="$HOST_X11" HOST_WAYLAND_DISPLAY="$HOST_WAYLAND" ./target/debug/penumbra-comp $COMPOSITOR_ARGS &
COMP_PID=$!

# Wait for Wayland socket to initialize
while [ ! -S "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET" ] && [ ! -S "/tmp/$PENUMBRA_SOCKET" ]; do
    sleep 0.1
done

# 2. Check and start desktop components on Output B (External Monitor)
BG_PID=""
PANEL_PID=""

if [ "$USE_COSMIC" = true ] && command -v cosmic-panel &> /dev/null; then
    echo "Starting COSMIC desktop components (cosmic-bg, cosmic-panel) on external monitor..."
    WAYLAND_DISPLAY="$PENUMBRA_SOCKET" cosmic-bg &
    BG_PID=$!
    WAYLAND_DISPLAY="$PENUMBRA_SOCKET" cosmic-panel &
    PANEL_PID=$!
    SHELL_ARGS="--phone-only"
else
    if [ "$USE_COSMIC" = true ]; then
        echo "Notice: cosmic-panel binary not found. Falling back to native Penumbra Convergence Desktop."
    else
        echo "Running native Penumbra Convergence Desktop on Output B (External Monitor)."
        if pgrep -x cosmic-panel >/dev/null 2>&1; then
            echo "Note: Active host COSMIC desktop session detected on this PC."
            echo "Running duplicate cosmic-panel/cosmic-bg inside a nested compositor would cause D-Bus single-instance conflicts."
            echo "Using native Penumbra Convergence Desktop on Output B for a rock-solid nested experience."
        fi
    fi
    SHELL_ARGS="--dual-screen"
fi

# 3. Start Shell
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" GDK_BACKEND=wayland QT_QPA_PLATFORM=wayland ./target/debug/penumbra-shell $SHELL_ARGS &
SHELL_PID=$!

trap "kill $COMP_PID $SHELL_PID $BG_PID $PANEL_PID 2>/dev/null || true" EXIT
wait $COMP_PID
