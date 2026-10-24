# `penumbra-keyboard` — On-Screen Layer-Shell Virtual Keyboard

`penumbra-keyboard` is a standalone, mobile-first on-screen virtual keyboard designed for the Penumbra Mobile Desktop Environment. Built in Rust with Slint declarative UI and `layer-shell-slint`, it adheres strictly to **Android 17 / Pixel Material You (Material 3/4 Expressive)** design principles.

---

## Key Features

1. **Native Wayland Layer-Shell Overlay:**
   - Composited via `wlr-layer-shell` on `Layer::Overlay`.
   - Anchored to `Bottom | Left | Right` across the 412px logical canvas width.
   - Configured with `KeyboardInteractivity::None`: Tapping keys on the virtual keyboard never steals input focus away from the active client application (e.g., terminal, web browser, settings search box).

2. **Zero-Overhead Linux Kernel Input (`/dev/uinput`):**
   - Creates a kernel-level virtual input device: `"Penumbra Virtual Keyboard"`.
   - Emits genuine Linux `EV_KEY` and `EV_SYN` input events.
   - Generates authentic scancodes (`KEY_A`..`KEY_Z`, `KEY_0`..`KEY_9`, `KEY_SPACE`, `KEY_ENTER`, `KEY_BACKSPACE`, etc.) with hardware Shift/Caps synchronization.
   - Works uniformly across all Wayland clients without needing client-side input method integration.
   - Graceful fallback: If `/dev/uinput` is unprivileged or unavailable, the application runs without panicking.

3. **Physical Hardware Keyboard Coexistence:**
   - Actively monitors Linux input devices (`/proc/bus/input/devices`).
   - Distinguishes genuine alphanumeric typing keyboards from power buttons, volume rockers, lid switches, and virtual devices.
   - **Auto-Hide:** Automatically retracts and disables the on-screen keyboard when a physical typing keyboard (USB, Bluetooth, or internal) is plugged in.
   - Automatically restores the virtual keyboard when the physical keyboard is disconnected.

4. **KDE-Compliant Desktop Specification:**
   - Shipped with `data/penumbra-keyboard.desktop`:
     ```ini
     [Desktop Entry]
     Name=Penumbra Keyboard
     Comment=Penumbra On-Screen Virtual Keyboard
     Exec=/usr/local/bin/penumbra-keyboard
     Icon=input-keyboard-virtual
     Terminal=false
     Type=Application
     NoDisplay=true
     X-KDE-Wayland-VirtualKeyboard=true
     Categories=Utility;Accessibility;
     ```
   - Uses `NoDisplay=true` to hide the daemon from general application grids.
   - Uses `X-KDE-Wayland-VirtualKeyboard=true` following the KDE Plasma / KWin Wayland standard for system virtual keyboard discovery.
   - Discovered and manageable through `penumbra-settings` under **Settings > Keyboard**.

5. **CLI and IPC Remote Control:**
   - Running `penumbra-keyboard` with control flags sends commands to the background daemon via `$XDG_RUNTIME_DIR/penumbra-keyboard-cmd`:
     ```bash
     # Toggle visibility
     penumbra-keyboard --toggle

     # Force show
     penumbra-keyboard --show

     # Force hide
     penumbra-keyboard --hide
     ```

---

## Material You Design Language

- **Deep Night Palette:** Base surface `#141218`, container squircles `#2b2930`, pressed feedback `#4a4852`.
- **Functional Key Elevation:** Shift, Backspace, Symbols mode in `#36343b`.
- **Action Enter Key:** Pastel accent blue `#a8c7fa` with deep navy text `#042b59`.
- **Header Bar:** Top drag pill handle (36px x 4px) with quick hide chevron button and keyboard status.
- **Floating Restore Pill:** When collapsed without a physical keyboard, a discreet 48x38px capsule with a keyboard icon remains accessible in the bottom corner.

---

## Directory Layout

```
crates/penumbra-keyboard/
├── Cargo.toml
├── build.rs
├── README.md
├── src/
│   ├── main.rs               # Layer-shell setup, Slint binding, lifecycle loop
│   ├── uinput.rs             # Linux /dev/uinput driver emitting evdev keycodes
│   ├── physical_detector.rs  # Detection and filtering of physical hardware keyboards
│   └── ipc.rs                # Polling for external CLI commands (show/hide/toggle)
└── ui/
    └── keyboard_window.slint # Declarative Slint UI with QWERTY, shift, and symbols
```
