# Penumbra DE / Mobile Shell - AI Guidelines & System Rules

## 1. Project Mission & Identity
Penumbra is an open-source, mobile-first Linux desktop environment and standalone nested Wayland compositor.
- **Functional Target:** Equivalent to Phosh, GNOME Mobile, and KDE Plasma Mobile (real Linux hardware integration, true Wayland client management, D-Bus session services, zero UI mocking).
- **Aesthetic Target:** Contemporary iOS and Android 15/16/17 (Material 3 Expressive, organic curves, squircle pills, 1px specular highlight rims, tactile drag physics, spring velocity overscroll).
- **Core Technology Stack:**
  - **Compositor Engine:** Smithay (Rust, DRM/KMS, EGL/GLES, multi-output).
  - **UI / Shell Layer:** Slint (Rust declarative UI).
  - **Protocols:** Wayland (`xdg-shell`, `wlr-layer-shell`, `ext-session-lock-v1`).

---

## 2. Hard Architectural Constraints (DO NOT VIOLATE)

### Rule 1: Custom Layer-Shell Support (No Monolithic Mock Canvas)
- Do NOT use standard Winit windowing to render a single monolithic application containing both system bars and desktop mockups.
- We are implementing our own native Wayland `layer-shell` support directly in the codebase.
- The Slint UI chrome components (Top Status Bar, Bottom Navigation Bar, Quick Settings Overlay, App Switcher, Lock Screen) must be implemented and composited as distinct, dedicated surfaces anchored via `wlr-layer-shell` semantics (`Layer::Top`, `Layer::Overlay`, `Layer::Background`).
- Slint must never act as a window manager. Third-party applications (`foot`, `chatty`, `firefox`, `calls`) communicate directly with the Smithay compositor via `xdg-shell`.

### Rule 2: Absolute Ban on Mock UI & Drawn Windows
- Under NO circumstance should any code generate Slint widgets that visually simulate terminal emulators, browser windows, or external screens as drawn rectangles inside a single UI tree.
- When an application is launched, it must spawn the actual installed binary with `WAYLAND_DISPLAY` injected, and its GPU buffer must be composited natively by Smithay.
- Applications in the launcher must be dynamically populated from real XDG desktop entries (`/usr/share/applications`, `~/.local/share/applications`, Flatpak paths).

### Rule 3: Mobile System Insets & Surface Geometry (Phone Mode: 412x915)
- **Top Status Bar:** Fixed height `42px`, solid background (`#000000`), anchored at `(0, 0)`.
- **Bottom Navigation Bar:** Fixed height `32px`, anchored at `(0, 883)`.
- **App Work Area:** Bounded strictly to `x: 0, y: 42, width: 412, height: 841`. Applications must never render behind opaque shell areas, nor hide the system bars unless entering an explicit fullscreen immersive state.

### Rule 4: Distinct Recents Overview (Not Phosh's Embedded App Grid)
- Do NOT merge running tasks into the launcher/homescreen like Phosh does.
- The Recents/App Switcher is an independent overlay state with horizontal preview cards.
- **Bottom gesture routing:**
  - Fast upward flick: Minimizes current app and reveals the clean Homescreen.
  - Slow upward drag and pause (> 80px hold): Enters the horizontal Recents carousel.
  - Horizontal swipe left / right on bottom bar: Instantly switches to adjacent running applications.

### Rule 5: Asymmetric Multi-Output (Convergence)
- **Output A (Phone Display):** Single-window fullscreen mobile policy (412x915).
- **Output B (External Display):** Floating/tiling desktop policy (1280x720 / 1920x1080) hosting standard desktop layer surfaces (panels, wallpaper, docks) via `wlr-layer-shell`.

---

## 3. Rust Code Standards
1. All generated Rust code must pass `cargo check --workspace` without compiler errors or unresolved borrows.
2. Maintain clean separation between the declarative UI (`.slint` files) and system services/Wayland IPC (`crates/`).
