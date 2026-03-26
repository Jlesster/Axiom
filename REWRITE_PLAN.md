# Axiom - Wayland Compositor

## Project Overview

**Axiom** is a modern, Lua-configurable Wayland compositor written in Rust, inspired by AwesomeWM's architecture and configuration paradigm. The project brings AwesomeWM's powerful scripting and customization capabilities to Wayland, leveraging modern Linux graphics APIs.

---

# IMPORTANT: Rewrite Plan

## Scope

A **complete rewrite** of the entire codebase is planned, keeping **only** the Lua API defined in `src/scripting/lua_api.rs`. All other components will be redesigned and reimplemented.

## What to Keep

- **Lua API** (`src/scripting/lua_api.rs`) - The complete Lua API with all functions:
  - Configuration: `axiom.set { border_width, gap, bar_height, ... }`
  - Spawning: `axiom.spawn(cmd)`
  - Keybinds: `axiom.key(combo, fn)`, `axiom.unkey(combo)`
  - Workspace management: `axiom.workspace(n)`, `axiom.send(n)`, `axiom.ws()`
  - Window management: `axiom.focus(dir)`, `axiom.move(dir)`, `axiom.close()`
  - Float/Fullscreen: `axiom.float()`, `axiom.fullscreen()`
  - Layout control: `axiom.layout(ws, name)`, `axiom.inc_master()`, `axiom.dec_master()`
  - Client queries: `axiom.clients()`, `axiom.focused()`, `axiom.screens()`
  - Rules: `axiom.rule { match, action }`
  - Signals: `axiom.on(event, fn)`, `axiom.off(event)`
  - Control: `axiom.reload()`, `axiom.quit()`

- **WmConfig structure** - The window manager configuration struct referenced by the Lua API
- **ActionQueue** - The action queue system for Lua-triggered compositor actions

## What to Rewrite

Everything else. The current implementation has fundamental architectural issues:

1. **State management is scattered** - Root state, window state, output state are interleaved
2. **Protocol handling is verbose** - Wayland protocol implementations are repetitive
3. **Rendering pipeline is complex** - OpenGL setup, texture management, shader compilation
4. **Backend initialization is fragile** - DRM/GBM/EGL initialization order matters
5. **No separation of concerns** - Core logic mixed with platform-specific code

---

# Current Architecture (Reference for Rewrite)

## Directory Structure

```
src/
├── main.rs              # Entry point, event loop, render orchestration
├── state.rs            # Root Axiom state struct
├── sys.rs              # Centralized libc FFI / syscall declarations
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

---

## Core Components

### 1. Entry Point (`src/main.rs`)

- Event loop setup using `calloop`
- Client connection handling via Wayland server
- Input event dispatching
- Render loop orchestration
- Signal handling (SIGTERM, SIGINT)

**Key structs:**
- `NoopClientData` - Empty client data implementation
- Main event loop with sources for: DRM events, input devices, Wayland clients, timers

### 2. Root State (`src/state.rs`)

The `Axiom` struct holds all compositor state:

```rust
pub struct Axiom {
    pub display: Display<NoopClientData>,
    pub loop_handle: LoopHandle<'static, Box<Axiom>>,
    pub backend: Backend,
    pub render: RenderState,
    pub inputs: HashMap<client_id, InputState>,
    pub wm: WmState,
    pub xwayland: XWaylandState,
    pub script: ScriptEngine,
    pub outputs: HashMap<OutputId, OutputState>,
    pub pending: HashMap<WlSurface, PendingSurface>,
    pub committed: HashMap<WlSurface, CommittedSurface>,
    pub ipc: IpcServer,
    pub focus_stack: Vec<client_id>,
    // ...
}
```

**Key types:**
- `OutputState` - Per-output state (name, dimensions, refresh, scale, render surface)
- `PendingSurface` - Uncommitted surface state (buffer, damage, callbacks)
- `CommittedSurface` - Committed surface state (buffer, dimensions)
- `RawBuffer` - Either SHM or DMABUF buffer representation

### 3. Backend (`src/backend/`)

Provides hardware abstraction:

**session.rs** - libseat integration
- `Session` - Wraps libseat connection
- VT switching support
- Device open/close

**drm.rs** - DRM device wrapper
- `DrmDevice` - Implements `drm::Device` and `drm::control::Device` traits
- Encoder/CRTC/connector enumeration
- Mode setting and page flipping

**gbm.rs** - GBM surface management
- `OutputSurface` - GBM surface + EGL surface + DRM state
- Buffer swapping and framebuffer management

**egl.rs** - EGL context
- `EglContext` - Global EGL state
- `EglSurface` - Per-output EGLSurface
- Window surface creation from GBM surface

### 4. Window Manager (`src/wm/`)

Pure window management logic (no Wayland dependencies):

**wm/mod.rs**
- `Rect` - Rectangle type with inset/center/contains operations
- `WindowId` - Unique window identifier
- `Window` - Window state (position, size, floating, fullscreen, maximized)
- `Workspace` - Collection of windows with layout
- `Monitor` - Physical monitor with workspaces
- `WmState` - Global WM state with monitors/workspaces/windows
- `WmConfig` - User configuration (border_width, gaps, etc.)

**wm/layout.rs**
- `Layout` enum: MasterStack, BSP, Monocle, Float
- Layout application to workspace

**wm/rules.rs**
- `WindowRule` - Match condition + effects
- `RuleEngine` - Apply rules to new windows

**wm/anim.rs**
- `AnimSet` - Animation state for window focus/movement
- Spring physics for smooth transitions

### 5. Rendering (`src/render/`)

OpenGL-based compositing:

**render/mod.rs**
- Shader programs: quad vertex, texture fragment, solid fragment, chrome fragment
- `RenderState` - Global render state with programs and texture cache
- Per-output rendering
- Layer shell surface rendering
- Window rendering (tiled and floating)
- Status bar rendering
- Cursor rendering

**render/programs.rs**
- `GlProgram` - Compiled shader program
- `GlTexture` - Uploaded client buffer texture
- `QuadVao` - Fullscreen quad geometry

**render/chrome.rs**
- Window decorations (borders, shadows)
- Rounded corners via fragment shader SDF
- Drop shadows

**render/bar.rs**
- Status bar rendering
- Catppuccin theme

**render/font.rs**
- FreeType font loading
- Glyph caching

**render/glyph_vao.rs**
- Text rendering VAO

**render/cursor.rs**
- Hardware cursor via DRM dumb buffer

### 6. Wayland Protocols (`src/proto/`)

All Wayland protocol implementations:

**Core protocols:**
- `compositor.rs` - wl_compositor, wl_surface, wl_subcompositor
- `shm.rs` - wl_shm, wl_shm_pool, buffer handling with mmap
- `wl_output.rs` - wl_output global
- `output.rs` - Per-output wl_output implementation

**Shell protocols:**
- `xdg_shell.rs` - xdg_wm_base, xdg_surface, xdg_toplevel, xdg_popup
- `xdg_output.rs` - zxdg_output_manager_v1
- `xdg_decoration.rs` - zxdg_decoration_manager_v1
- `layer_shell.rs` - zwlr_layer_shell_v1

**Input:**
- `seat.rs` - wl_seat, wl_keyboard, wl_pointer, wl_touch

**Utilities:**
- `dmabuf.rs` - zwp_linux_dmabuf_v1
- `screencopy.rs` - zwlr_screencopy_manager_v1
- `fractional_scale.rs` - wp_fractional_scale_manager_v1
- `idle_inhibit.rs` - zwp_idle_inhibit_manager_v1

### 7. Input (`src/input/`)

libinput integration:

**input/mod.rs**
- `InputState` - Per-client input state
- Keyboard handling via xkbcommon
- Pointer handling
- Touch handling
- libseat integration (conflicts with session.rs)

### 8. Scripting (`src/scripting/`)

Lua configuration engine:

**scripting/mod.rs**
- `ScriptEngine` - Lua interpreter wrapper
- Config file loading from `~/.config/axiom/axiom.rc.lua`
- Signal handling

**scripting/lua_api.rs** - **KEEP THIS**
- Complete Lua API as documented above
- `ActionQueue` for deferred action execution

**scripting/signals.rs**
- AwesomeWM-compatible signal system
- `client.focus`, `client.open`, `client.close`, etc.

**scripting/abi.rs** - Stub for C ABI plugins

### 9. IPC (`src/ipc/`)

External tool communication:

**ipc/mod.rs**
- `IpcServer` - Unix socket server
- Command handling

**ipc/commands.rs**
- Control commands for axiomctl

### 10. XWayland (`src/xwayland/`)

X11 window support:

**xwayland/mod.rs**
- `XWaylandState` - X11 connection state
- X11 event handling

**xwayland/wm.rs**
- `X11WmState` - X11 window manager
- EWMH support (_NET_WM_STATE, etc.)
- Size hints, decorations

**xwayland/surface.rs**
- `XwaylandSurface` wrapper

**xwayland/atoms.rs**
- X11 atom definitions

### 11. Portal (`src/portal/`)

xdg-desktop-portal integration (broken/incomplete):

**portal/mod.rs** - Portal orchestration
**portal/dbus.rs** - D-Bus communication
**portal/pipewire_stream.rs** - PipeWire stream handling

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
- `x11rb` v0.13 - X11 connection handling
- `nix` v0.31 - Unix syscall wrappers

### Scripting
- `mlua` v0.9 (Lua 5.4, vendored)

### Utilities
- `calloop` v0.12 - Event loop
- `tracing` v0.1 - Logging

---

## Known Issues (for Rewrite Reference)

1. **Missing Cargo.lock** - Dependencies not locked
2. **Duplicate libseat definitions** - Conflicting FFI in session.rs and input/mod.rs
3. **Window content not rendering** - Client buffers not appearing
4. **Chrome rendering incomplete** - Only half visible
5. **CSD app handling** - Client-decorated apps not handled correctly
6. **Configure sequencing** - Race conditions with window configures

---

## Configuration

### Location
```
~/.config/axiom/axiom.rc.lua
```

### Example
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

-- Window rules
axiom.rule { match = { app_id = "firefox" }, action = { workspace = 2 } }
```

---

## Building

```bash
cargo build --release
sudo make install  # Installs to /usr/local/bin/axiom
```

### Running
```bash
axiom
# Or with socket:
WAYLAND_DISPLAY=wayland-axiom axiom
```

### Logging
```bash
AXIOM_LOG=debug,axiom=trace cargo run --release
```