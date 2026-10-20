#!/usr/bin/env bash
set -e

# Capture host display server variables
HOST_WAYLAND="${WAYLAND_DISPLAY:-wayland-1}"
HOST_X11="${DISPLAY:-:0}"
PENUMBRA_SOCKET="wayland-penumbra-bar-0"

echo "🔨 Compiling penumbra-comp and penumbra-top-bar..."
cargo build --bin penumbra-comp --bin penumbra-top-bar

# Clean up any stale sockets
rm -f "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET"* "/tmp/$PENUMBRA_SOCKET"* 2>/dev/null || true

echo "🚀 Starting Penumbra Compositor on host display..."
WAYLAND_DISPLAY="$HOST_WAYLAND" DISPLAY="$HOST_X11" HOST_WAYLAND_DISPLAY="$HOST_WAYLAND" \
    ./target/debug/penumbra-comp --socket "$PENUMBRA_SOCKET" &
COMP_PID=$!

# Wait for Wayland socket to appear
while [ ! -S "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET" ] && [ ! -S "/tmp/$PENUMBRA_SOCKET" ]; do
    sleep 0.1
done

echo "📱 Launching Penumbra Top Bar via layer-shell-slint on $PENUMBRA_SOCKET..."
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" ./target/debug/penumbra-top-bar &
BAR_PID=$!

trap "kill $COMP_PID $BAR_PID 2>/dev/null || true" EXIT
echo "✨ Status Bar is active! Press Ctrl+C to close."
wait $COMP_PID
