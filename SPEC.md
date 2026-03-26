# Axiom - Wayland Compositor Specification

## Project Overview

**Axiom** is a modern, Lua-configurable Wayland compositor written in Rust. It is inspired by AwesomeWM's architecture and configuration paradigm, bringing powerful scripting and customization capabilities to Wayland.

### Core Design Principles

1. **Single-threaded event loop**: All compositor logic runs in one calloop event loop
2. **Lua-driven configuration**: Users configure everything via Lua scripts
3. **Hardware-accelerated rendering**: Uses DRM/GBM/EGL for GPU-accelerated compositing
4. **Modular protocol handlers**: Wayland protocols implemented as separate modules

### Target Users
- Linux desktop users who want AwesomeWM-like configurability on Wayland
- Developers building custom Wayland compositors
- Users who want deep customization through Lua scripting

---

## Architecture Overview

```
src/
├── main.rs              # Entry point, event loop setup
├── state.rs             # Root Axiom state struct
├── sys.rs               # FFI/syscall declarations
├── backend/             # Hardware abstraction (DRM/GBM/EGL)
├── proto/               # Wayland protocol implementations
├── render/              # OpenGL compositing
├── scripting/           # Lua engine and API
├── input/               # libinput handling
├── wm/                  # Window manager logic
├── ipc/                 # Unix socket IPC server
├── portal/              # xdg-desktop-portal integration
└── xwayland/           # XWayland support
```

---

## Dependencies (from Cargo.toml)

### Graphics Stack
- `drm` v0.12 - Direct Rendering Manager
- `drm-fourcc` v2 - FourCC codes for buffer formats
- `gbm` v0.15 - Graphics Buffer Manager
- `khronos-egl` v6 - EGL interface (dynamic loading)
- `gl` v0.14 - OpenGL bindings
- `libloading` v0.8 - Dynamic library loading

### Wayland
- `wayland-server` v0.31
- `wayland-protocols` v0.31 (server, unstable, staging features)
- `wayland-protocols-wlr` v0.2 (server feature)

### Input
- `input` v0.8 - libinput
- `xkbcommon` v0.7 (wayland feature)

### Event Loop
- `calloop` v0.12 (signals feature)
- `calloop-wayland-source` v0.2

### Scripting
- `mlua` v0.9 (lua54, vendored features)

### Utilities
- `tracing` v0.1 - Logging
- `tracing-subscriber` v0.3 (env-filter feature)
- `log` v0.4
- `anyhow` v1 - Error handling
- `bytemuck` v1 - Type utilities
- `serde` + `serde_json` - Serialization
- `x11rb` v0.13.2 - X11 connection
- `nix` v0.31.2 (socket, fs, process features)
- `fcntl` v0.1.0
- `libc` v0.2.183

---

## Module Specifications

### 1. Main Entry Point (main.rs)

**Responsibilities:**
- Initialize logging (AXIOM_LOG env var)
- Create Wayland Display
- Create calloop EventLoop
- Build Axiom state
- Register display fd with calloop
- Load Lua config (non-fatal if missing)
- Run event loop with drain callback

**Key Code Pattern:**
```rust
fn main() -> anyhow::Result<()> {
    // Logging setup
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("AXIOM_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // Wayland display
    let display: Display<Axiom> = Display::new()?;
    let dh = display.handle();

    // Event loop
    let mut event_loop: EventLoop<Axiom> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();
    let loop_signal = event_loop.get_signal();

    // Build state
    let mut state = Axiom::new(display, loop_handle.clone(), loop_signal, &dh)?;

    // Register display fd
    loop_handle.insert_source(
        calloop::generic::Generic::new(
            state.display_fd(),
            calloop::Interest::READ,
            calloop::Mode::Level,
        ),
        |_, _, state: &mut Axiom| {
            state.dispatch_clients()?;
            Ok(calloop::PostAction::Continue)
        },
    )?;

    // Load config (non-fatal)
    if let Err(e) = state.script.load_config(&mut state.wm) {
        tracing::warn!("Config error: {e}");
    }

    // Run loop
    event_loop.run(
        Some(std::time::Duration::from_millis(8)), // ~120 Hz
        &mut state,
        |state| {
            state.flush_clients();
            state.drain_actions();
        },
    )?;

    Ok(())
}
```

---

### 2. State (state.rs)

**Root state struct:**
```rust
pub struct Axiom {
    // Wayland
    pub display: Display<Axiom>,
    pub dh: DisplayHandle,

    // calloop
    pub loop_handle: LoopHandle<'static, Axiom>,
    pub loop_signal: LoopSignal,

    // Sub-systems
    pub backend: Backend,
    pub render: RenderState,
    pub input: InputState,
    pub wm: WmState,
    pub script: ScriptEngine,

    // Wayland globals
    pub globals: Globals,

    // State flags
    pub needs_redraw: bool,
    pub running: Arc<AtomicBool>,
}
```

**Key methods:**
- `new()` - Initialize all subsystems
- `display_fd()` - Return fd for calloop
- `dispatch_clients()` - Dispatch Wayland clients
- `flush_clients()` - Flush outgoing events
- `drain_actions()` - Drain Lua action queue, trigger render
- `sync_keyboard_focus()` - Update keyboard focus after WM changes
- `close_window(id)` - Request client close, remove from WM
- `send_configure_focused()` - Send configure to focused window
- `reload_config()` - Reload Lua config

---

### 3. Backend (backend/)

**Responsibilities:** Hardware abstraction, GPU/display initialization

#### 3.1 Session (session.rs)
- Uses libseat for session management (VT switching, device permissions)
- Opens DRM device via session
- Dispatches seat events

**Key FFI declarations:**
```rust
extern "C" {
    fn libseat_open_seat(listener: *const SeatListener, userdata: *mut libc::c_void) -> *mut libseat_seat;
    fn libseat_close_seat(seat: *mut libseat_seat) -> libc::c_int;
    fn libseat_open_device(seat: *mut libseat_seat, path: *const libc::c_char, fd: *mut libc::c_int) -> libc::c_int;
    fn libseat_close_device(seat: *mut libseat_seat, device_id: libc::c_int) -> libc::c_int;
    fn libseat_dispatch(seat: *mut libseat_seat, timeout: libc::c_int) -> libc::c_int;
}
```

#### 3.2 DRM Device (drm.rs)
- Opens DRM node via session
- Enumerates connectors, encoders, CRTCs, modes
- Sets DRM master
- Page flip support

**Key types:**
```rust
pub struct DrmDevice {
    fd: OwnedFd,
    _device_id: i32,
}

pub struct ConnectorInfo {
    pub connector: connector::Handle,
    pub crtc: crtc::Handle,
    pub mode: drm::control::Mode,
    pub width_mm: u32,
    pub height_mm: u32,
}
```

#### 3.3 GBM Device (gbm.rs)
- Creates GBM device from DRM fd
- Allocates scanout surfaces
- Format: XRGB8888, SCANOUT | RENDERING flags

#### 3.4 EGL Context (egl.rs)
- Loads libEGL dynamically
- Creates GBM platform display
- OpenGL ES 2 context
- Per-output EGL surfaces

#### 3.5 Output Surface (output.rs)
- Ties together GBM surface, EGL surface, DRM CRTC
- Front/back buffer tracking for page flipping
- Initial black frame, then page flip

**Present pattern:**
```rust
pub fn present(&mut self, drm: &DrmDevice, egl: &EglContext) -> Result<()> {
    egl.swap_buffers(&self.egl_surface)?;
    let bo = self.gbm_surface.lock_front_buffer()?;
    let fb = add_framebuffer(drm, &bo)?;
    drm.page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)?;
    // Release old fb
    if let Some(old) = self.front_fb.replace(fb) {
        drm.destroy_framebuffer(old).ok();
    }
    Ok(())
}
```

**Init pattern:**
```rust
// Initial black frame
egl.make_current(&egl_surface)?;
gl::ClearColor(0.0, 0.0, 0.0, 1.0);
gl::Clear(gl::COLOR_BUFFER_BIT);
egl.swap_buffers(&egl_surface)?;

// Lock and create fb
let bo = gbm_surface.lock_front_buffer()?;
let fb = add_framebuffer(drm, &bo)?;

// Set CRTC
drm.set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))?;
```

---

### 4. Window Manager (wm/)

**Responsibilities:** Window state, workspaces, layouts, tiling geometry

#### 4.1 Core Types

```rust
pub type WindowId = u32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self;
    pub fn contains(&self, px: i32, py: i32) -> bool;
    pub fn inset(&self, amount: i32) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    #[default] MasterStack,  // Classic master + stack
    Bsp,                       // Binary space partition
    Monocle,                   // Fullscreen focused only
    Float,                     // Floating windows
}
```

#### 4.2 Window
```rust
pub struct Window {
    pub id: WindowId,
    pub app_id: String,
    pub title: String,
    pub rect: Rect,
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub workspace: usize,
    saved_rect: Option<Rect>,
}
```

#### 4.3 Workspace
```rust
pub struct Workspace {
    pub windows: Vec<WindowId>,
    pub focused: Option<WindowId>,
    pub layout: Layout,
    pub master_count: usize,
    pub master_ratio: f32, // [0.1, 0.9]
}
```

#### 4.4 Monitor
```rust
pub struct Monitor {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub active_ws: usize,
    pub bar_height: i32,
}
```

#### 4.5 WmConfig (Lua-settable)
```rust
pub struct WmConfig {
    pub border_w: u32,          // 2
    pub gap: u32,               // 6
    pub outer_gap: u32,         // 0
    pub bar_height: u32,        // 24
    pub workspaces_count: usize, // 9
    pub bar_at_bottom: bool,   // false
    pub active_border: [f32; 4], // blue
    pub inactive_border: [f32; 4], // dark gray
    pub bar_bg: [f32; 4],       // dark blue
}
```

#### 4.6 WmState
```rust
pub struct WmState {
    pub config: WmConfig,
    pub workspaces: Vec<Workspace>,
    pub windows: HashMap<WindowId, Window>,
    pub monitors: Vec<Monitor>,
    active_ws: usize,
    next_id: WindowId,
}
```

**Key methods:**
- `new()` - Create with default config, 9 workspaces
- `active_ws()` - Get active workspace index
- `focused_window()` - Get focused window id
- `add_window()` - Allocate new window, add to active ws
- `remove_window(id)` - Remove window, reflow
- `focus_window(id)` - Focus window, switch ws if needed
- `focus_direction(dir)` - Focus nearest window in direction (0=left,1=right,2=up,3=down)
- `move_direction(dir)` - Swap with nearest window in direction
- `switch_workspace(ws)` - Switch active workspace
- `move_to_workspace(id, ws)` - Move window to workspace
- `fullscreen_window(id, on)` - Toggle fullscreen
- `set_title(id, title)` - Update window title
- `set_app_id(id, app_id)` - Update window app_id
- `inc_master()` / `dec_master()` - Adjust master count
- `reflow()` - Recompute all tiling geometry
- `apply_config(cfg)` - Apply new config, adjust workspaces
- `add_monitor(x,y,w,h)` - Add monitor, return index
- `remove_monitor(idx)` - Remove monitor

#### 4.7 Layout Algorithms

**MasterStack:**
```rust
// First master_count windows fill left column at master_ratio width
// Remaining windows evenly stacked in right column
```

**BSP:** Recursive halving, alternating horizontal/vertical split

**Monocle:** All windows occupy full area, only focused visible

**Float:** Windows not tiled, use stored floating rect

---

### 5. Rendering (render/)

**Responsibilities:** OpenGL compositing, frame presentation

#### 5.1 RenderState

```rust
pub struct RenderState {
    gl_ready: bool,
}
```

**Key methods:**
- `new(backend)` - Initialize GL from first output's EGL surface
- `render_frame(backend, wm)` - Render all outputs

**Render pipeline:**
```rust
fn render_workspace(wm: &WmState, ws_idx: usize, out_w: u32, out_h: u32) {
    // 1. Clear with background color
    gl::Viewport(0, 0, out_w as i32, out_h as i32);
    gl::ClearColor(bg[0], bg[1], bg[2], bg[3]);
    gl::Clear(gl::COLOR_BUFFER_BIT);

    // 2. Draw tiled windows (or focused only for monocle)
    for &id in workspace.windows {
        // Draw border (if not fullscreen)
        if border_w > 0 && !win.fullscreen {
            fill_rect(win.rect, border_color, ...);
        }
        // Draw window body (inset by border)
        fill_rect(win.rect.inset(border_w), window_color, ...);
    }

    // 3. Draw status bar
    fill_rect(bar_rect, bar_bg, ...);
}
```

**Helper for solid fills (using scissor):**
```rust
fn fill_rect(rect: Rect, color: [f32; 4], vp_w: i32, vp_h: i32) {
    // Convert to GL coords (origin bottom-left)
    let scissor_y = vp_h - rect.y - rect.h;
    gl::Enable(gl::SCISSOR_TEST);
    gl::Scissor(rect.x, scissor_y, rect.w, rect.h);
    gl::ClearColor(color[0], color[1], color[2], color[3]);
    gl::Clear(gl::COLOR_BUFFER_BIT);
    gl::Disable(gl::SCISSOR_TEST);
}
```

---

### 6. Input (input/)

**Responsibilities:** libinput event handling, keyboard state, pointer tracking

#### 6.1 libinput Interface

```rust
struct InputInterface;
impl LibinputInterface for InputInterface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<i32, i32>;
    fn close_restricted(&mut self, fd: i32);
}
```

#### 6.2 Keyboard State
```rust
struct KeyboardState {
    context: xkb::Context,
    keymap: xkb::Keymap,
    state: xkb::State,
}
```

**Key methods:**
- `process_key(keycode, direction)` - Update modifier state, return UTF-8
- `combo_for_key(keycode)` - Build "super+shift+a" style combo string

#### 6.3 Pointer State
```rust
struct PointerState {
    pub x: f64,
    pub y: f64,
}
```

#### 6.4 InputState

```rust
pub struct InputState {
    libinput: Libinput,
    keyboard: KeyboardState,
    pointer: PointerState,
    pub focus: FocusedSurface, // keyboard/pointer focus WindowId
}
```

**Key methods:**
- `new(loop_handle)` - Initialize libinput, register fd with calloop
- `dispatch_events(state)` - Drain and process libinput events
- `set_keyboard_focus(id, dh, wm)` - Sync keyboard focus after WM change
- `keymap_string()` - Serialize keymap for wl_keyboard
- `modifier_state()` - Get current modifiers
- `pointer_pos()` - Get current pointer position

**Event handling pattern:**
```rust
fn handle_event(&mut self, event: input::Event, state: &mut Axiom) {
    match event {
        Event::Keyboard(KeyboardEvent::Key(ev)) => {
            let keycode = ev.key();
            let dir = match ev.key_state() {
                KeyState::Pressed => xkb::KeyDirection::Down,
                KeyState::Released => xkb::KeyDirection::Up,
            };
            let _utf8 = self.keyboard.process_key(keycode, dir);
            if dir == xkb::KeyDirection::Down {
                let combo = self.keyboard.combo_for_key(keycode);
                let handled = state.script.fire_keybind(&combo);
                if !handled {
                    tracing::trace!("unhandled key: {combo}");
                }
            }
        }
        Event::Pointer(PointerEvent::Motion(ev)) => {
            self.pointer.x = (self.pointer.x + ev.dx()).max(0.0);
            self.pointer.y = (self.pointer.y + ev.dy()).max(0.0);
            self.update_pointer_focus(state);
        }
        Event::Pointer(PointerEvent::Button(ev)) => {
            if ev.button_state() == ButtonState::Pressed {
                // Click-to-focus
                if let Some(id) = hit_test(&state.wm, px as i32, py as i32) {
                    state.wm.focus_window(id);
                    state.sync_keyboard_focus();
                    state.needs_redraw = true;
                }
            }
        }
        // ... other events
    }
}
```

**Hit test helper:**
```rust
fn hit_test(wm: &WmState, px: i32, py: i32) -> Option<WindowId> {
    // Test focused window first, then iterate from top of stack
}
```

---

### 7. Scripting (scripting/)

**Responsibilities:** Lua VM, API, config loading, action queue

#### 7.1 Action types
```rust
pub enum LuaAction {
    Spawn(String),
    FocusId(WindowId),
    CloseId(WindowId),
    MoveToWorkspace(WindowId, usize),
    SwitchWorkspace(usize),
    SetLayout(usize, Layout),
    SetFloat(WindowId, bool),
    SetFullscreen(WindowId, bool),
    SetWindowTitle(WindowId, String),
    IncMaster,
    DecMaster,
    Reload,
    Quit,
}

pub type ActionQueue = Arc<Mutex<Vec<LuaAction>>>;
```

#### 7.2 ScriptEngine

```rust
pub struct ScriptEngine {
    pub lua: Lua,
    pub actions: ActionQueue,
}
```

**Key methods:**
- `new()` - Create Lua VM and action queue
- `load_config(wm)` - Install API, load ~/.config/axiom/axiom.rc.lua
- `drain(state)` - Drain action queue, apply actions
- `emit_client_open(wm, id)` - Fire client.open signal
- `emit_client_close(wm, id)` - Fire client.close signal
- `emit_client_focus(wm, id)` - Fire client.focus signal
- `emit_bare(event)` - Fire signal with no argument
- `fire_keybind(combo)` - Fire keybind handler, return handled bool

**Config path:** `$XDG_CONFIG_HOME/axiom/axiom.rc.lua` or `~/.config/axiom/axiom.rc.lua`

#### 7.3 Lua API (lua_api.rs)

Install in Lua:

```lua
-- axiom.set { ... }
axiom.set {
    border_width = 2,
    gap = 6,
    bar_height = 24,
    workspaces = 9,
    bar_at_bottom = false,
    border_active = "#7aa2f7",
    border_inactive = "#3b4261",
    bar_bg = "#1e1e2e",
}

-- axiom.spawn(cmd)
axiom.spawn("alacritty")

-- axiom.notify(msg [, ms])
axiom.notify("Hello", 3000)

-- Keybinds
axiom.key("super+return", function()
    axiom.spawn("alacritty")
end)
axiom.unkey("super+return")

-- Workspaces
axiom.workspace(n)  -- switch to workspace n
axiom.send(n)       -- move focused to workspace n
axiom.ws()          -- current workspace (1-based)
axiom.layout(ws, name) -- set layout: "master_stack", "bsp", "monocle", "float"

-- Focus/movement
axiom.focus("left")   -- direction: left/right/up/down
axiom.cycle(+1)      -- cycle focus +1 or -1
axiom.move("left")   -- move window in direction

-- Window actions
axiom.close()
axiom.float()        -- toggle float
axiom.fullscreen()   -- toggle fullscreen
axiom.inc_master()   -- grow master count
axiom.dec_master()   -- shrink master count

-- Query
axiom.clients()      -- list of client tables
axiom.focused()      -- focused client table or nil
axiom.screens()      -- list of monitor tables

-- Rules
axiom.rule {
    match = { app_id = "firefox" },
    action = { workspace = 2, float = true }
}

-- Signals
axiom.on("client.open", function(c) end)
axiom.on("client.close", function(c) end)
axiom.on("client.focus", function(c) end)
axiom.on("compositor.ready", function() end)
axiom.off("client.open")  -- clear handlers

-- Compositor
axiom.reload()  -- reload config
axiom.quit()    -- exit
```

**Client table fields:**
- id, app_id, title, floating, fullscreen, maximized
- x, y, width, height
- Methods: close(), focus(), move_to(ws)

---

### 8. Protocol Handlers (proto/)

**Responsibilities:** Wayland protocol implementations

#### 8.1 Registration (proto/mod.rs)

All globals created at startup:
```rust
pub fn register_globals(dh: &DisplayHandle) {
    dh.create_global::<Axiom, WlCompositor, _>(6, ());
    dh.create_global::<Axiom, WlSubcompositor, _>(1, ());
    dh.create_global::<Axiom, WlShm, _>(1, ());
    dh.create_global::<Axiom, WlOutput, _>(4, ());
    dh.create_global::<Axiom, WlSeat, _>(7, ());
    dh.create_global::<Axiom, XdgWmBase, _>(5, ());
    dh.create_global::<Axiom, ZwlrLayerShellV1, _>(4, ());
    dh.create_global::<Axiom, ZxdgDecorationManagerV1, _>(1, ());
    dh.create_global::<Axiom, ZxdgOutputManagerV1, _>(3, ());
    dh.create_global::<Axiom, ZwpLinuxDmabufV1, _>(3, ());
    dh.create_global::<Axiom, ZwlrScreencopyManagerV1, _>(3, ());
    dh.create_global::<Axiom, WpFractionalScaleManagerV1, _>(1, ());
    dh.create_global::<Axiom, WpViewporter, _>(1, ());
    dh.create_global::<Axiom, ZwpIdleInhibitManagerV1, _>(1, ());
}
```

#### 8.2 Compositor (proto/compositor.rs)

**Surface data:**
```rust
pub struct SurfaceData {
    pub pending: Mutex<PendingSurfaceState>,
    pub current: Mutex<CommittedSurfaceState>,
    pub children: Mutex<Vec<WlSurface>>,
    pub parent: Mutex<Option<WlSurface>>,
    pub role: Mutex<SurfaceRole>,
    pub viewport: Mutex<Option<ViewportState>>,
}

pub struct PendingSurfaceState {
    pub buffer: Option<Option<WlBuffer>>,
    pub dx: i32,
    pub dy: i32,
    pub damage_surface: Vec<Rect>,
    pub damage_buffer: Vec<Rect>,
    pub frame_callbacks: Vec<WlCallback>,
    pub input_region: Option<Option<RegionData>>,
    pub opaque_region: Option<Option<RegionData>>,
    pub buffer_scale: Option<i32>,
    pub buffer_transform: Option<wl_output::Transform>,
}

pub struct CommittedSurfaceState {
    pub buffer: Option<WlBuffer>,
    pub dx: i32,
    pub dy: i32,
    pub damage_buffer: Vec<Rect>,
    pub frame_callbacks: Vec<WlCallback>,
    pub input_region: Option<RegionData>,
    pub opaque_region: Option<RegionData>,
    pub buffer_scale: i32,
    pub buffer_transform: Option<wl_output::Transform>,
    pub needs_upload: bool,
}
```

**Surface role:**
```rust
pub enum SurfaceRole {
    None,
    XdgToplevel,
    XdgPopup,
    LayerSurface,
    Subsurface,
    Cursor,
    DnDIcon,
}
```

**Key handlers:**
- `CreateSurface` - Create surface with SurfaceData
- `CreateRegion` - Create region with RegionData
- `Attach` - Store buffer in pending
- `Damage` / `DamageBuffer` - Store damage
- `Frame` - Add frame callback
- `SetInputRegion` / `SetOpaqueRegion` - Store regions
- `SetBufferScale` / `SetBufferTransform` - Store transform
- `Commit` - Commit surface (call state.on_surface_commit)

#### 8.3 XDG Shell (proto/xdg_shell.rs)

**Configure sequence:**
1. `GetToplevel` → send configure(0,0,[]) + xdg_surface.configure(serial)
2. Client commits wl_surface → state.on_surface_commit → wm.add_window() + reflow()
3. Send configure(w,h,states) + xdg_surface.configure(serial)
4. Client acks → configured = true, ever_acked = true
5. Client commits content → upload texture

**XDG surface data:**
```rust
pub struct XdgSurfaceData {
    pub wl_surface: WlSurface,
    pub configured: bool,
    pub ever_acked: bool,
    pub configure_serial: u32,
    pub role: XdgRole,
}
```

**Toplevel data:**
```rust
pub struct ToplevelData {
    pub xdg_surface: XdgSurface,
    pub xdg_data: XdgSurfaceDataRef,
    pub window_id: Option<WindowId>,
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    pub pending_states: Vec<xdg_toplevel::State>,
}
```

**Key handlers:**
- `GetToplevel` - Create toplevel, initial configure(0,0,[])
- `SetTitle` - Update window title
- `SetAppId` - Update window app_id
- `SetParent` - Set window parent
- `SetMinSize` / `SetMaxSize` - Size constraints
- `Move` / `Resize` - Interactive move/resize
- `SetMaximized` / `UnsetMaximized` - Toggle maximize
- `SetFullscreen` / `UnsetFullscreen` - Toggle fullscreen
- `SetMinimized` - Minimize window
- `AckConfigure` - Mark configured
- `SetWindowGeometry` - Currently ignored (compositor owns rect)
- `Destroy` - Remove window from WM

**Configure with states:**
```rust
fn send_configure_toplevel(state: &mut Axiom, toplevel: &XdgToplevel, data: &ToplevelDataRef) {
    let (width, height, states) = if win_id > 0 {
        let win = state.wm.window(win_id);
        let mut st = vec![];
        if win.maximized { st.push(State::Maximized); }
        if win.fullscreen { st.push(State::Fullscreen); }
        if focused { st.push(State::Activated); }
        if tiled { st.push(State::TiledLeft); st.push(State::TiledRight); st.push(State::TiledTop); st.push(State::TiledBottom); }
        (win.rect.w, win.rect.h, st)
    } else {
        (0, 0, vec![])
    };
    toplevel.configure(width, height, states_bytes);
    xdg_surface.configure(serial);
}
```

#### 8.4 Other Protocols

- **Seat** - wl_seat with keyboard/pointer
- **SHM** - wl_shm for shared memory buffers
- **Layer Shell** - zwlr_layer_shell_v1 for status bar
- **XDG Output** - zxdg_output_manager_v1
- **XDG Decoration** - zxdg_decoration_manager_v1
- **DMABuf** - zwp_linux_dmabuf_v1
- **Screencopy** - zwlr_screencopy_manager_v1
- **Fractional Scale** - wp_fractional_scale_manager_v1
- **Idle Inhibit** - zwp_idle_inhibit_v1

---

### 9. IPC (ipc/)

**Responsibilities:** Unix socket control interface

**Socket path:** `$XDG_RUNTIME_DIR/axiom-<display>.sock`

**Wire format:** One newline-terminated JSON request in, one response out

**Request/Response types:**
```rust
enum IpcRequest {
    Clients,
    Workspaces,
    Monitors,
    ActiveWindow,
    Version,
    CloseWindow { id: Option<WindowId> },
    FocusWindow { id: Option<WindowId> },
    MoveToWorkspace { workspace: usize },
    ToggleFloat,
    ToggleFullscreen,
    ToggleMaximize,
    SetWindowGeometry { id: Option<WindowId>, x: i32, y: i32, w: i32, h: i32 },
    SwitchWorkspace { workspace: usize },
    SetLayout { layout: String },
    Reload,
    Exec { command: String },
    Exit,
    Lua { code: String },
    Bind { key: String },
}
```

---

### 10. XWayland (xwayland/)

**Responsibilities:** Spawn and manage XWayland, translate X11 events to Wayland

**Key types:**
```rust
pub struct XWaylandState {
    pub child: Option<Child>,
    pub conn: Option<Arc<RustConnection>>,
    pub display: Option<u32>,
    pub wm: Option<X11WmState>,
    pub x11_to_wl: HashMap<u32, WindowId>,
    pub wl_to_x11: HashMap<WindowId, u32>,
    pub pending_surfaces: Vec<XwaylandSurface>,
    pub x11_serials: HashMap<u32, u64>,
    pub ready: bool,
}
```

**Actions:**
```rust
enum X11Action {
    MapWindow { x11_win: u32, title: String, app_id: String, override_redirect: bool, surface_serial: Option<u64> },
    UnmapWindow { x11_win: u32 },
    ConfigureRequest { x11_win: u32, x: Option<i32>, y: Option<i32>, w: Option<u32>, h: Option<u32> },
    TitleChanged { x11_win: u32, title: String },
    FocusRequest { x11_win: u32 },
}
```

---

### 11. Portal (portal/)

**Responsibilities:** xdg-desktop-portal integration for screenshots/screencast

Contains D-Bus communication and PipeWire stream handling stubs.

---

## Build and Run

### Build
```bash
cargo build --release
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

## Config Location

`~/.config/axiom/axiom.rc.lua`

---

## Implementation Status

### Complete
- DRM/GBM/EGL rendering pipeline with page flipping
- Multi-monitor with automatic detection
- Tiling layouts: MasterStack, BSP, Monocle, Float
- Lua configuration engine with AwesomeWM-compatible API
- Status bar with layer shell
- libinput keyboard/pointer handling
- Workspace management (9 workspaces per output)
- Window rules engine
- Spring-based animations (stub)
- VT switching via libseat
- XWayland integration
- IPC server (axiomctl)
- xdg-desktop-portal integration (screenshots)
- Idle inhibit protocol
- Scratchpad support (structure exists)
- Window decorations (border rendering)
- EWMH support
- xdg-decoration protocol
- Pointer constraints protocol
- Relative pointer protocol

### Incomplete/Issues
- Window content rendering (client buffers not appearing)
- Chrome rendering (partial)
- DMABuf rendering (implemented but may have issues)
- Fractional scaling (protocol registered, not fully utilized)
- Plugin ABI (stub only)
- Xcursor (returns generated arrow)
- Drag-and-drop (exists, no handler)

---

## Key Patterns

**Event Loop Integration:**
```rust
loop_handle.insert_source(
    Generic::new(fd, Interest::READ, Mode::Level),
    |_, _, state: &mut Axiom| {
        state.dispatch_clients()?;
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

**Surface Commit Flow:**
```rust
fn commit_surface(state: &mut Axiom, surface: &WlSurface, data: &Arc<SurfaceData>) {
    // Move pending to current
    // Release old buffer
    // Add frame callbacks
    // Signal needs_upload if new buffer
    state.on_surface_commit(surface);
}
```

**Window Lifecycle:**
1. Client creates wl_surface
2. Client creates xdg_surface → get toplevel
3. Initial configure(0,0,[]) sent
4. Client commits wl_surface (on_surface_commit)
5. wm.add_window() allocates window id
6. reflow() computes tiling rect
7. Second configure(rect.w, rect.h, states) sent
8. Client acks configure (configured = true)
9. Client commits content (texture upload)
10. Render draws window

---

## Rust Tooling

```bash
cargo fmt
cargo clippy
cargo check
cargo test  # if tests exist
```

---

## Notes for Rebuild

1. Start with backend (session → drm → gbm → egl → output)
2. Add Wayland display and event loop
3. Implement protocol globals registration
4. Add compositor surface handling
5. Add XDG shell handling
6. Add WM state and tiling layouts
7. Add rendering (solid colors first, textures later)
8. Add input handling
9. Add scripting engine
10. Add IPC and XWayland last