#!/usr/bin/env bash
set -e

# Capture host display server variables so the compositor window opens on your host desktop
HOST_WAYLAND="${WAYLAND_DISPLAY:-wayland-1}"
HOST_X11="${DISPLAY:-:0}"
PENUMBRA_SOCKET="wayland-penumbra-0"

cargo build --workspace

# Clean up any stale sockets and lock files
rm -f "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET"* "/tmp/$PENUMBRA_SOCKET"* 2>/dev/null || true

# 1. Start Penumbra Compositor
COMPOSITOR_ARGS="--socket $PENUMBRA_SOCKET"
WAYLAND_DISPLAY="$HOST_WAYLAND" DISPLAY="$HOST_X11" HOST_WAYLAND_DISPLAY="$HOST_WAYLAND" ./target/debug/penumbra-comp $COMPOSITOR_ARGS &
COMP_PID=$!

# Wait for Wayland socket to initialize
while [ ! -S "$XDG_RUNTIME_DIR/$PENUMBRA_SOCKET" ] && [ ! -S "/tmp/$PENUMBRA_SOCKET" ]; do
    sleep 0.1
done

# 2. Start Penumbra Shell (dual-screen mode by default, or pass custom args)
SHELL_ARGS="${*:-"--dual-screen"}"
WAYLAND_DISPLAY="$PENUMBRA_SOCKET" GDK_BACKEND=wayland QT_QPA_PLATFORM=wayland ./target/debug/penumbra-shell $SHELL_ARGS &
SHELL_PID=$!

trap "kill $COMP_PID $SHELL_PID 2>/dev/null || true" EXIT
wait $COMP_PID
