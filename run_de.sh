#!/usr/bin/env bash
set -e

# ==============================================================================
# Penumbra Linux Mobile Desktop Environment - Standalone Session Runner
#
# Runs the entire Penumbra DE inside a single nested host window:
# - Starts Smithay Wayland Compositor (penumbra-comp)
# - Launches the Top Status Bar (penumbra-top-bar via layer-shell-slint)
# - Cleans up all child processes on exit (Ctrl+C or window close)
#
# Usage:
#   ./run_de.sh          # Phone-only mode (412x915 smartphone window)
#   ./run_de.sh --dual   # Dual-screen convergence mode (Phone + External Monitor)
# ==============================================================================

MODE="phone"
for arg in "$@"; do
    case "$arg" in
        --dual|--convergence)
            MODE="dual"
            ;;
        --phone|--mobile)
            MODE="phone"
            ;;
    esac
done

HOST_WAYLAND="${WAYLAND_DISPLAY:-wayland-1}"
HOST_X11="${DISPLAY:-:0}"
PENUMBRA_SOCKET="wayland-penumbra-session-0"

echo "=========================================================="
echo "🌟 Penumbra Linux Mobile Desktop Environment"
echo "   Mode: $MODE"
echo "   Socket: $PENUMBRA_SOCKET"
echo "=========================================================="

echo "🔨 Building Penumbra components..."
cargo build --bin penumbra-comp --bin penumbra-top-bar --bin penumbra-homescreen --bin penumbra-nav-bar --bin penumbra-quick-settings

# Clean up any stale sockets
rm -f "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET"* "/tmp/$PENUMBRA_SOCKET"* 2>/dev/null || true

# 1. Start Compositor
COMP_ARGS="--nested --socket $PENUMBRA_SOCKET"
if [ "$MODE" = "dual" ]; then
    COMP_ARGS="$COMP_ARGS --convergence"
fi

echo "🚀 Starting Penumbra Compositor window..."
WAYLAND_DISPLAY="$HOST_WAYLAND" DISPLAY="$HOST_X11" HOST_WAYLAND_DISPLAY="$HOST_WAYLAND" \
    ./target/debug/penumbra-comp $COMP_ARGS &
COMP_PID=$!

# Wait for Wayland socket to appear
while [ ! -S "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET" ] && [ ! -S "/tmp/$PENUMBRA_SOCKET" ]; do
    sleep 0.1
done

echo "📱 Launching Penumbra Homescreen (Layer::Background)..."
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" ./target/debug/penumbra-homescreen &
HOME_PID=$!

echo "📱 Launching Penumbra Top Status Bar (Layer::Top)..."
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" ./target/debug/penumbra-top-bar &
TOP_BAR_PID=$!

echo "📱 Launching Penumbra Navigation Bar (Layer::Top)..."
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" ./target/debug/penumbra-nav-bar &
NAV_BAR_PID=$!

echo "📱 Launching Penumbra Quick Settings (Layer::Overlay)..."
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" ./target/debug/penumbra-quick-settings &
QS_PID=$!

# Child processes array for graceful cleanup
PIDS=($COMP_PID $HOME_PID $TOP_BAR_PID $NAV_BAR_PID $QS_PID)

cleanup() {
    echo ""
    echo "🛑 Shutting down Penumbra DE session..."
    kill "${PIDS[@]}" 2>/dev/null || true
    rm -f "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET"* "/tmp/$PENUMBRA_SOCKET"* 2>/dev/null || true
    echo "👋 Session terminated cleanly."
}
trap cleanup EXIT INT TERM

echo ""
echo "✅ Penumbra DE is running!"
echo "   👉 To launch any Wayland app inside Penumbra, open another terminal and run:"
echo "      WAYLAND_DISPLAY=$PENUMBRA_SOCKET <app_name> (e.g. foot, gedit, firefox)"
echo ""
echo "Press Ctrl+C to stop the session."
wait $COMP_PID
