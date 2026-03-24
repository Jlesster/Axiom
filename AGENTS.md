# Axiom - Wayland Compositor Agent Documentation

## Project Overview

**Axiom** is a modern, Lua-configurable Wayland compositor written in Rust, inspired by AwesomeWM's architecture and configuration paradigm. The project aims to bring AwesomeWM's powerful scripting and customization capabilities to Wayland, leveraging modern Linux graphics APIs while maintaining a clean, maintainable codebase.

**Key Goals:**
- AwesomeWM-compatible configuration model via Lua scripting
- Hardware-accelerated rendering with DRM/GBM/EGL
- Clean, modular Rust codebase
- Full Wayland protocol support
- Multi-monitor and workspace management

---

## Architecture

```
src/
├── main.rs              # Entry point, event loop, render orchestration
├── state.rs             # Root Axiom state struct
├── sys.rs               # Centralized libc FFI / syscall declarations
├── ipc/                 # IPC server for external tools
│   ├── mod.rs
│   └── commands.rs
├── portal/              # xdg-desktop-portal integration
│   ├── mod.rs           # Portal orchestration
│   ├── dbus.rs          # D-Bus communication
│   └── pipewire_stream.rs # PipeWire stream handling
├── backend/             # Hardware abstraction layer
│   ├── mod.rs           # Backend orchestration
│   ├── drm.rs           # DRM device, CRTC, page flipping
│   ├── gbm.rs           # GBM surface management
│   ├── egl.rs           # EGL context, OpenGL setup
│   └── session.rs       # libseat session management (VT switching)
├── proto/               # Wayland protocol implementations
│   ├── mod.rs           # Global registry
│   ├── compositor.rs    # wl_compositor, wl_surface, wl_subcompositor
│   ├── xdg_shell.rs     # xdg_wm_base, xdg_surface, xdg_toplevel, xdg_popup
│   ├── seat.rs          # wl_seat, wl_keyboard, wl_pointer
│   ├── shm.rs           # wl_shm, wl_shm_pool
│   ├── layer_shell.rs   # zwlr_layer_shell_v1
│   ├── wl_output.rs     # wl_output
│   ├── output.rs        # wl_output per-output state
│   ├── xdg_output.rs   # zxdg_output_manager_v1
│   ├── xdg_decoration.rs # zxdg_decoration_manager_v1
│   ├── idle_inhibit.rs  # zwp_idle_inhibit_v1
│   ├── dmabuf.rs        # zwp_linux_dmabuf_v1
│   ├── screencopy.rs    # zwlr_screencopy_manager_v1
│   └── fractional_scale.rs # wp_fractional_scale_manager_v1
├── render/              # OpenGL compositing
│   ├── mod.rs           # Render loop, output rendering
│   ├── programs.rs      # Shader compilation, VAO/VBO
│   ├── bar.rs           # Status bar rendering
│   ├── cursor.rs        # Hardware cursor via DRM dumb buffer
│   ├── font.rs          # FreeType glyph atlas
│   └── glyph_vao.rs     # Streaming VAO for text
├── scripting/           # Lua configuration engine
│   ├── mod.rs           # ScriptEngine, config loading
│   ├── lua_api.rs       # Complete Lua API
│   ├── signals.rs       # AwesomeWM-compatible signals
│   └── abi.rs           # C ABI for plugins (stub)
├── input/               # Input handling
│   └── mod.rs           # libinput dispatch, keybinds, pointer
├── wm/                  # Window manager logic
│   ├── mod.rs           # Core WM: windows, workspaces, monitors
│   ├── layout.rs        # Tiling layouts
│   ├── rules.rs         # Window rule matching
│   └── anim.rs          # Spring physics animations
└── xwayland/            # XWayland integration
    ├── mod.rs           # XWayland manager, X11 event handling
    ├── atoms.rs         # X11 atom definitions
    ├── surface.rs       # XwaylandSurface wrapper
    └── wm.rs            # X11WmState with EWMH, size hints, decorations
```

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `backend` | DRM device access, GBM buffers, EGL context, session management |
| `proto` | Wayland protocol implementations for client communication |
| `render` | OpenGL compositing, shaders, text rendering, status bar |
| `scripting` | Lua engine, API bindings, signal system |
| `input` | libinput integration, keyboard/pointer handling, keybinds |
| `wm` | Window management, workspaces, layouts, rules, animations |
| `ipc` | IPC server for external tool communication |
| `portal` | xdg-desktop-portal integration for screenshots/screencast |
| `xwayland` | XWayland window manager integration |
| `sys` | Centralized libc FFI declarations (mmap, memfd, etc.) |

---

## Dependencies

### Graphics Stack
- `drm` v0.12 - Direct Rendering Manager
- `gbm` v0.15 - Graphics Buffer Manager
- `khronos-egl` v6 - EGL interface
- `gl` v0.14 - OpenGL bindings

### Wayland
- `wayland-server` v0.31
- `wayland-protocols` v0.31
- `wayland-protocols-wlr` v0.2

### Input
- `input` v0.8 - libinput
- `xkbcommon` v0.7

### X11
- `x11rb` - X11 connection handling
- `nix` - Unix syscall wrappers

### Scripting
- `mlua` v0.9 (Lua 5.4, vendored)

### Utilities
- `calloop` v0.12 - Event loop
- `tracing` v0.1 - Logging

---

## Configuration

### Location
```
~/.config/axiom/axiom.rc.lua
```

### Lua API

```lua
-- Configuration
axiom.config({
    border_width = 2,
    gap = 6,
    workspaces = 9,
})

-- Keybinds
axiom.bind("Super+Return", function()
    axiom.spawn("alacritty")
end)

-- Window management
axiom.clients()           -- All windows
axiom.focused()          -- Focused window
axiom.close()            -- Close focused
axiom.float()            -- Toggle float
axiom.fullscreen()       -- Toggle fullscreen

-- Workspaces
axiom.workspace(n)       -- Workspace object
axiom.active_workspace() -- Current index

-- Layout
axiom.inc_master()
axiom.dec_master()

-- Rules
axiom.rule { match = { app_id = "firefox" }, action = { workspace = 2 } }

-- Signals
axiom.on("client.focus", function(c) end)
axiom.on("client.open", function(c) end)
axiom.on("compositor.ready", function() end)
```

### AwesomeWM Compatibility
- Global tables: `client`, `tag`, `screen`, `awful`
- Keybinding format: `"Super+Return"`, `"Mod4+Shift+q"`
- Signal names: `client.open`, `client.close`, `client.focus`

---

## Development Guidelines

### Code Style
- Rust 2021 edition
- Use `tracing` for logging (see `AXIOM_LOG` env var)
- Error handling via `anyhow::Result`
- Avoid unsafe code except in FFI/drm bindings

### Adding New Features

1. **Wayland Protocols**: Add to `src/proto/`
   - Register globals in `proto/mod.rs`
   - Implement handlers in appropriate module

2. **Window Management**: Add to `src/wm/mod.rs`
   - State structures in `WmState`
   - Logic in appropriate submodule

3. **Lua API**: Add to `src/scripting/lua_api.rs`
   - Register function in `ScriptEngine::new`
   - Follow existing patterns

4. **Rendering**: Add to `src/render/`
   - Shaders in `programs.rs`
   - Rendering logic in `mod.rs`

### Key Patterns

**Event Loop Integration:**
```rust
event_loop.handle().insert_source(
    Generic::new(fd, Interest::READ, Mode::Level),
    |_, _, state| {
        // Handle events
        Ok(PostAction::Continue)
    },
)?;
```

**Render Pipeline:**
```rust
// 1. Clear framebuffer
// 2. Draw layer shell surfaces
// 3. Draw tiled windows
// 4. Draw floating windows
// 5. Draw status bar
// 6. Page flip
```

---

## Building and Running

### Build
```bash
cargo build --release
```

### Install
```bash
sudo make install  # Installs to /usr/local/bin/axiom
```

### Run
```bash
axiom
# Or with socket:
WAYLAND_DISPLAY=wayland-axiom axiom
```

### Logging
```bash
AXIOM_LOG=debug,axiom=trace cargo run --release
```

---

## Known Implementation Status

### Complete
- DRM/GBM/EGL rendering pipeline with proper page flipping
- Multi-monitor with automatic detection
- Tiling layouts: MasterStack, BSP, Monocle, Float
- Lua configuration engine with AwesomeWM-compatible API
- Status bar with Catppuccin theme using layer shell
- libinput keyboard/pointer handling with keybinds
- Workspace management (9 workspaces per output)
- Window rules engine with float/workspace/size/position effects
- Spring-based animations for window focus/movement
- VT switching via libseat
- XWayland integration (spawn, surface pairing, event handling, EWMH)
- IPC server with control commands (axiomctl)
- xdg-desktop-portal integration (screenshots via zwp_linux_dmabuf_v1)
- Idle inhibit protocol (zwp-idle-inhibit-v1)
- Scratchpad support
- Window decorations with rounded corners and drop shadows
- EWMH support (_NET_WM_STATE, window types, etc.)
- xdg-decoration protocol (server-side decorations)
- Pointer constraints protocol
- Relative pointer protocol

### Partial/Incomplete
- DMABuf rendering (implemented but may have issues with some clients)
- Fractional scaling (protocol registered, not fully utilized)
- Drag-and-drop (DnDIcon exists, no handler)
- Plugin ABI (stub only)
- Xcursor loading (returns generated arrow)

---

## Current Working Topic: Window Content and Chrome Rendering Issues

### Issue: No Window Content Rendering + Partial Chrome Rendering

**Problem Description:**
Windows are being assigned correct tiles and chrome decorations are being drawn, but:
- Window content (client buffers) is not rendering at all
- Only half the window chrome is rendering (likely missing bottom half or one side)
- Textures from clients appear to not be uploading correctly or at all

**Current Symptoms:**
- Window borders/shadows may render but client content is invisible
- Some chrome elements visible, others missing
- Likely a texture upload or shader sampling issue

**Root Causes (Suspected):**

1. **Texture Upload Failure**: Client buffer textures are not being correctly uploaded to OpenGL:
   - SHM buffers may not be properly mapped to textures
   - DMAbuf textures may be incorrectly configured
   - Texture sampling coordinates may be inverted or misaligned

2. **Chrome Fragment Shader Issues**: Half the chrome rendering suggests:
   - Vertex data may be incomplete or using wrong winding order
   - Fragment shader may have incorrect alpha blending
   - Corner radius or shadow calculations may be clipping incorrectly

3. **Configure Sequencing**: When a new window is added:
   - Initial 0×0 configure is sent to let client pick size
   - Client responds with its preferred size
   - Layout assigns a rect based on tiling
   - Second configure is sent with the layout rect
   - This can cause a race condition if the client commits before receiving the second configure

4. **Surface Pairing Timing**: For XWayland:
   - X11 window appears and must be paired with a Wayland surface
   - WL_SURFACE_SERIAL mechanism requires careful synchronization
   - Late pairing can cause geometry issues

5. **Client-Size vs Compositor-Size Mismatch**:
   - Clients with CSD (GTK4, Qt6) report inner content size, not including decorations
   - The compositor expects surface size to match the assigned rect
   - `set_window_geometry` is called but may not handle CSD apps correctly

**Affected Files:**
- `src/render/mod.rs` — texture upload, chrome rendering, fragment shaders
- `src/render/programs.rs` — shader compilation, VAO/VBO setup
- `src/wm/mod.rs` — `set_window_geometry`, `reflow`
- `src/state.rs` — `on_surface_commit`, `send_configure_for_surface`
- `src/proto/xdg_shell.rs` — configure sequencing, geometry handling
- `src/proto/shm.rs` — SHM buffer to texture conversion

**Potential Fixes:**

1. **Debug Texture Upload**: Add logging to verify texture dimensions and data after upload.

2. **Chrome Shader Inspection**: Review corner radius SDF calculations and shadow offset math in fragment shader.

3. **Vertex Data Validation**: Ensure all 4 corners of chrome quad are being rendered, check for degenerate triangles.

4. **For CSD Apps**: Implement proper handling of `xdg_toplevel.set_window_geometry` to account for client-reported inner geometry vs assigned outer geometry.

5. **Configure Coalescing**: Combine the initial and layout-based configures into a single configure sent after the window is added to the WM.

6. **XWayland Surface Pairing**: Improve serial tracking and pairing logic to ensure windows are properly matched before geometry assignment.

---

## Future Direction

### Short-term
- [ ] Fix window content rendering (client buffers not appearing)
- [ ] Fix chrome rendering (only half visible)
- [ ] Fix CSD application handling
- [ ] Implement DMABuf for hardware-accelerated client buffers
- [ ] Add fractional scaling support
- [ ] Improve screen capture integration

### Medium-term
- [ ] Plugin system via C ABI
- [ ] IPC commands documentation
- [ ] Enhanced animation system
- [ ] Multi-GPU support

### Long-term
- [ ] Workspace binding per output
- [ ] Window snapping guides
- [ ] Comprehensive hotkey daemon
- [ ] XDG portal integration (full)

---

## Key Files Reference

| File | Purpose |
|------|---------|
| `src/state.rs` | Root `Axiom` state struct definition |
| `src/main.rs` | Entry point, event loop, render orchestration |
| `src/wm/mod.rs` | Core window/workspace/monitor state |
| `src/scripting/lua_api.rs` | Complete Lua API implementation |
| `src/render/mod.rs` | Main rendering pipeline |
| `src/proto/xdg_shell.rs` | Window management protocol |
| `src/ipc/mod.rs` | IPC server implementation |
| `src/portal/mod.rs` | Portal integration |
| `src/xwayland/mod.rs` | XWayland manager |

---

## Rust Tooling

```bash
# Format
cargo fmt

# Lint
cargo clippy

# Check
cargo check

# Test (if tests exist)
cargo test
```
