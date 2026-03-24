# Axiom Compositor - Implementation Roadmap

This document catalogs all missing, stubbed, and incomplete features in Axiom, organized by category with sufficient context for an agent to begin implementation immediately.

---

## Implementation Status Summary

| Category | Total Items | Completed | In Progress | Missing |
|----------|-------------|-----------|-------------|---------|
| Rendering Pipeline | 6 | 1 | 2 | 3 |
| Wayland Protocols | 5 | 4 | 0 | 1 |
| XWayland Integration | 3 | 2 | 1 | 0 |
| Window Management | 6 | 3 | 1 | 2 |
| Input Handling | 4 | 2 | 1 | 1 |
| Portal Integration | 2 | 1 | 1 | 0 |
| Lua API & Scripting | 4 | 2 | 1 | 1 |
| IPC Server | 2 | 2 | 0 | 0 |
| Backend & Graphics | 4 | 2 | 1 | 1 |
| Code Quality | 4 | 2 | 1 | 1 |
| **Total** | **40** | **21 (52%)** | **9 (23%)** | **10 (25%)** |

### Recently Completed (since 2026-03-23)
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

### Priority Fixes (CRITICAL)
1. **No window content rendering** - Client buffers not appearing in rendered windows
2. **Half chrome rendering** - Window decorations only partially visible (missing bottom half or one side)
3. **Output hotplug** - Can't detect monitor changes

---

## Logical Next Steps

### Step 1: Fix Window Content Rendering (CRITICAL)
Client buffers are not appearing in rendered windows.

**Check these files:**
| File | What to Verify | Lines |
|------|----------------|-------|
| `src/render/mod.rs` | `draw_surface()` texture upload path | ~300-500 |
| `src/proto/shm.rs` | SHM buffer to texture conversion | 1-250 |
| `src/proto/dmabuf.rs` | DMAbuf buffer import | 1-300 |
| `src/backend/egl.rs` | EGL texture creation | 100-425 |

**Debug commands:**
```bash
# Run with trace logging
AXIOM_LOG=debug,axiom=trace cargo run --release 2>&1 | grep -i texture

# Test with simple SHM app
WAYLAND_DISPLAY=wayland-axiom weston-flower
```

**Suspected Issues:**
- SHM buffers may not be properly mapped to textures
- Texture sampling coordinates may be inverted or misaligned
- DMAbuf textures may be incorrectly configured

---

### Step 2: Fix Chrome Rendering (CRITICAL)
Window decorations only rendering half (likely missing bottom half or one side).

**Check these files:**
| File | What to Verify | Lines |
|------|----------------|-------|
| `src/render/mod.rs` | `draw_chrome()` or chrome shader path | ~200-400 |
| `src/render/programs.rs` | Chrome fragment shader | 1-379 |

**Suspected Issues:**
- Vertex data may be incomplete or using wrong winding order
- Fragment shader may have incorrect alpha blending
- Corner radius or shadow calculations may be clipping incorrectly

---

### Step 3: Verify XWayland Surface Pairing Works
Required for production use with monitor changes.

**Check these files for compatibility:**
| File | What to Verify | Lines |
|------|----------------|-------|
| `src/backend/drm.rs` | Device enumeration | 1-100 |
| `src/backend/session.rs` | libseat event handling | 50-150 |
| `src/state.rs` | Monitor add/remove | 1-100 |

**Key functions to implement:**
```rust
// In src/backend/drm.rs
fn enumerate_connectors(&mut self) -> Vec<Connector>;
fn on_hotplug(&mut self) -> Vec<OutputChange>;
```

---

### Step 4: Implement Missing EWMH Properties
For better X11 app compatibility.

**Check these files for compatibility:**
| File | What to Verify | Lines |
|------|----------------|-------|
| `src/xwayland/wm.rs` | Property readers | 157-223 |
| `src/xwayland/atoms.rs` | Atom definitions | 1-50 |

**Implement in order:**
1. `_NET_WM_STATE` - Fullscreen/maximize state
2. `_NET_WM_WINDOW_TYPE` - Window type detection
3. `_NET_WM_ICON` - Window icons (optional)
4. `WM_SIZE_HINTS` - Size constraints

---

### Step 5: Touch & Gesture Support
For tablet/touchscreen users.

**Check these files for compatibility:**
| File | What to Verify | Lines |
|------|----------------|-------|
| `src/input/mod.rs` | libinput event handling | 100-300 |
| `src/proto/seat.rs` | wl_touch protocol | 1-50 |

---

## Compatibility Checklist

Before each release, verify these work:

### Core Functionality
- [x] XWayland windows appear and are tileable
- [ ] Native Wayland apps render correctly (content not visible)
- [x] Keyboard input works in all apps
- [x] Mouse/pointer works correctly
- [x] Multi-monitor setup detected at startup
- [x] Window decorations render (but partially - chrome missing half)

### Window Management
- [x] All 4 layouts work (MasterStack, BSP, Monocle, Float)
- [x] Window rules apply correctly
- [x] Focus cycling works
- [x] Scratchpad toggles
- [x] Animations play smoothly
- [x] Float/fullscreen states work

### Scripts & IPC
- [x] Lua config loads without errors
- [x] Keybinds execute correctly
- [x] IPC commands respond
- [x] Signals fire correctly

### Status Bar
- [x] Workspace tags display correctly
- [x] CPU/Memory widgets update
- [x] Clock widget shows time
- [x] Clicking tags switches workspace

---

## File Change Priority Matrix

| Priority | Files | Reason |
|----------|-------|--------|
| **P0-Critical** | `render/mod.rs`, `render/programs.rs` | No window content + partial chrome |
| **P1-High** | `proto/shm.rs`, `state.rs` | Texture upload issues |
| **P2-Medium** | `xwayland/wm.rs`, `drm.rs` | X11/Display issues |
| **P3-Low** | `input/mod.rs`, `portal/*.rs` | Input/Portal gaps |

---

---

## Table of Contents

1. [Rendering Pipeline](#1-rendering-pipeline)
2. [Wayland Protocols](#2-wayland-protocols)
3. [XWayland Integration](#3-xwayland-integration)
4. [Window Management](#4-window-management)
5. [Input Handling](#5-input-handling)
6. [Portal Integration](#6-portal-integration)
7. [Lua API & Scripting](#7-lua-api--scripting)
8. [IPC Server](#8-ipc-server)
9. [Backend & Graphics](#9-backend--graphics)
10. [Code Quality](#10-code-quality)
11. [Documentation](#11-documentation)

---

## 1. Rendering Pipeline

### 1.1 Window Content Rendering (CRITICAL - Broken)

**Status:** Client buffers not appearing  
**Files:** `src/render/mod.rs`, `src/proto/shm.rs`, `src/proto/dmabuf.rs`

**Problem:** Windows are tiled correctly and chrome decorations are drawn, but client content (the actual application window) does not render. This affects both SHM and DMABuf buffers.

**Current Symptoms:**
- Window borders/shadows render but client content invisible
- Textures from clients not uploading correctly or at all

**Suspected Root Causes:**
1. **Texture Upload Failure**: SHM/DMABuf buffers not correctly mapped to OpenGL textures
2. **Texture Sampling Issues**: Coordinates may be inverted or misaligned
3. **Configure Sequencing Race**: Client commits before receiving correct configure

**Debug Approach:**
1. Add logging to verify texture dimensions after upload
2. Check if texture data is actually being written
3. Verify shader sampler uniforms are correctly bound

---

### 1.2 Window Chrome Rendering (CRITICAL - Partial)

**Status:** Only half the chrome rendering  
**Files:** `src/render/mod.rs`, `src/render/programs.rs`

**Problem:** Window decorations (borders, shadows, title area) only render partially - likely missing bottom half or one side.

**Suspected Root Causes:**
1. **Vertex Data Incomplete**: Not all 4 corners of chrome quad being rendered
2. **Fragment Shader Issues**: Incorrect alpha blending or SDF calculations
3. **Corner Radius Clipping**: Shadow offset math causing incorrect clipping

**Debug Approach:**
1. Review vertex data for chrome quad (6 vertices = 2 triangles)
2. Check winding order and face culling
3. Verify fragment shader corner radius SDF calculations

---

### 1.3 DMABuf Rendering (Needs Verification)

**Status:** Implemented, may need debugging  
**Files:** `src/proto/dmabuf.rs`, `src/render/programs.rs`

**Problem:** DMABuf protocol is registered and may receive buffer parameters, but content not rendering.

**Expected Behavior:** Client DMA-BUF buffers should be imported as OpenGL textures via EGL DMA-BUF extension, allowing zero-copy GPU buffer sharing.

**Implementation Notes:**
- Requires `EGL_EXT_image_dma_buf_import` and `EGL_EXT_image_dma_buf_import_modifiers`
- Need to create `EglImage` from DMA-BUF file descriptors
- Must track image lifetime and destroy on surface commit
- Wire the imported texture ID into the render pipeline's `draw_surface()` call

**Reference:** Look at `egl.rs:import_dmabuf()` and `programs.rs` texture creation pattern.

---

### 1.2 Hardware Cursor Themes

**Status:** Stubbed  
**Files:** `src/render/cursor.rs`

**Problem:** `load_xcursor_pixels()` always returns `None`, falling back to generated arrow.

**Current Code:**
```rust
pub fn load_xcursor_pixels(&self, name: &str, size: u32) -> Option<(Vec<u8>, u32, u32)> {
    // TODO: Implement xcursor loading
    None
}
```

**Expected Behavior:** Load Xcursor themes from standard paths (`~/.icons`, `/usr/share/icons`) and render themed cursors.

**Implementation Notes:**
- Parse Xcursor manifest files (`index.theme`, `*.cursor`)
- Load PNG/XPM cursor images at requested sizes
- Map cursor names: "default" → arrow, "pointer" → hand, etc.
- Fallback to generated arrow if theme unavailable

---

### 1.3 Blur & Visual Effects

**Status:** Missing  
**Files:** `src/render/programs.rs`, `src/render/mod.rs`

**Problem:** No shader-based blur, shadows, or vignette effects.

**Expected Features:**
- Gaussian blur for window shadows
- Backdrop blur for layer shell surfaces (frosted glass effect)
- Vignette shader for ambiance
- Gradient borders (per-window configurable)

**Implementation Notes:**
- Add blur shader program using framebuffer ping-pong
- Implement two-pass Gaussian blur (horizontal + vertical)
- Add shadow rendering pass before window compositing
- Use `gl.BlitFramebuffer` for efficient texture copying

**Reference:** Hyprland's `SHADERS` system for blur kernels and shadow rendering.

---

### 1.4 Fractional Scaling

**Status:** Registered, unused  
**Files:** `src/proto/fractional_scale.rs`, `src/state.rs`

**Problem:** Protocol is globally registered but `ViewportState` is never used. No per-surface scale handling.

**Current State:**
```rust
// src/proto/fractional_scale.rs exists with ViewportState
// but never applied to surfaces
```

**Expected Behavior:** Surfaces requesting fractional scale (e.g., 1.5x, 1.25x) should be rendered at appropriate scale with proper viewport bounds.

**Implementation Notes:**
- Track `preferred_scale` per surface in `SurfaceState`
- Apply scale transformation when rendering surface textures
- Use `wp_viewporter` protocol for viewport clipping
- Handle scale changes on surface commit

---

### 1.5 Surface Damage Tracking

**Status:** Collected but unused  
**Files:** `src/proto/compositor.rs`, `src/render/mod.rs`

**Problem:** Damage regions are collected in `SurfaceState::damage` but never used for efficient partial redraws.

**Expected Behavior:** Only redraw damaged regions instead of full framebuffer clear.

**Implementation Notes:**
- Store damage rects in surface state
- On render, compute bounding box of damage
- Use `gl.Scissor` to limit draw area
- Clear only damaged regions before compositing

---

### 1.6 Color Management

**Status:** Missing  
**Files:** None (feature not started)

**Expected Behavior:** Proper color space handling (sRGB, HDR PQ/HLG), output color calibration.

**Implementation Notes:**
- Track surface color space via protocol (if exists) or assume sRGB
- Apply OETF (Opto-Electronic Transfer Function) in fragment shader
- Support HDR metadata via `_hdr_output_metadata` DRM property
- Configure output color space based on monitor EDID/EDR data

---

## 2. Wayland Protocols

### 2.1 Pointer Constraints

**Status:** Complete ✓  
**Files:** `src/proto/pointer_constraints.rs` (if exists)

**Implemented:**
- `zwp_pointer_constraints_v1` global
- `lock_pointer()` and `confine_pointer()` handlers
- Constraint state tracking in seat

---

### 2.2 Relative Pointer

**Status:** Complete ✓  
**Files:** `src/proto/relative_pointer.rs` (if exists)

**Implemented:**
- `zwp_relative_pointer_manager_v1` global
- Relative motion event generation

---

### 2.3 Virtual Keyboards

---

### 2.2 Virtual Keyboards

**Status:** Missing  
**Files:** None

**Expected Protocol:** `zwp_virtual_keyboard_v1`

**Required Implementation:**
- `VirtualKeyboardManager` global
- Per-client virtual keyboard creation
- `VirtualKeyboard` input injection
- Handle key events from virtual keyboard clients

---

### 2.3 Drag-and-Drop

**Status:** Partial  
**Files:** `src/proto/compositor.rs`

**Problem:** `SurfaceRole::DnDIcon` exists but no handler. No `wl_data_device_manager` implementation.

**Required Implementation:**
- `zwp_linux_dmabuf_v1` for drag icons (if not using SHM)
- `wl_data_source` for drag source
- `wl_data_offer` for drag target
- `wl_data_device` for clipboard/drag coordination
- Handle `drag.enter`, `drag.motion`, `drag.drop`, `drag.leave`

**Reference:** Look at `layer_shell.rs` for global registration pattern.

---

### 2.4 Text Input (IME)

**Status:** Missing  
**Files:** None

**Expected Protocols:**
- `zwp_text_input_v3` - Primary IME protocol
- `zwp_input_method_v2` - IME hub for accessibility

**Required Implementation:**
- `TextInputManager` global
- Manage focused text input surfaces
- Send `enter`, `leave`, `surrounding_text`, `content_hint`, `content_purpose`
- Receive `commit_string`, `delete_surrounding_text`, `cursor_position`

---

### 2.5 XDG Output Logical Size

**Status:** Protocol registered, no active implementation  
**Files:** `src/proto/xdg_output.rs`

**Problem:** Protocol registered in `proto/mod.rs` but logical size not set on outputs.

**Expected Behavior:** Report logical output size (physical size / scale) to clients.

**Implementation Notes:**
- Set `logical_width` and `logical_height` on output in `WlOutput`
- Calculate from physical dimensions and current scale
- Update when scale changes

---

## 3. XWayland Integration

### 3.1 Surface Pairing

**Status:** Implemented ✓  
**Files:** `src/xwayland/mod.rs`, `src/xwayland/surface.rs`, `src/state.rs`

**Current Implementation:**
- `try_pair_xwayland_surface()` - IMPLEMENTED
- Serial-based pairing via `WL_SURFACE_SERIAL` property
- `complete_pairing()` creates Window and adds to WM state
- Both X11-first and Wayland-first arrival paths handled

---

### 3.2 X11 Property Handling

**Status:** Complete ✓  
**Files:** `src/xwayland/wm.rs`

**Implemented Properties:**
- `WM_CLASS` - For rule matching (app_id) ✓
- `_NET_WM_NAME` / `WM_NAME` - Window title ✓
- `WL_SURFACE_SERIAL` - For surface pairing ✓
- `_NET_WM_STATE` - Fullscreen, above, below states ✓
- `_NET_WM_WINDOW_TYPE` - Window type for rules ✓

---

### 3.3 X11 Window Decorations

**Status:** Implemented ✓  

**Implemented:**
- Server-side window decorations with rounded corners
- Drop shadows around windows
- CSD detection via `_MOTIF_WM_HINTS`

---

## 4. Window Management

### 4.1 Window Groups (Tabbed Mode)

**Status:** Missing  
**Files:** None

**Expected Behavior:** Group windows into tabbed containers with tab bar.

**Lua API:**
```lua
axiom.group()           -- Group focused window
axiom.ungroup()        -- Ungroup focused window
axiom.group_next()     -- Next tab in group
axiom.group_prev()     -- Previous tab in group
```

**Implementation Notes:**
- New `WindowGroup` struct containing vector of windows
- Only one window visible at a time (others hidden)
- Render tab bar in decoration area
- Group state persisted per workspace

**Reference:** Hyprland's window groups implementation.

---

### 4.2 Window Snapping Guides

**Status:** Missing  
**Files:** None

**Expected Behavior:** Visual guides when dragging windows to screen edges/corners.

**Implementation Notes:**
- Detect when window is dragged near edge
- Show guide line at snap position
- Snap window to guide when released
- Support corner snapping (left-half, right-half, quarter)

---

### 4.3 Window Rules Engine

**Status:** Complete ✓  
**Files:** `src/wm/rules.rs`

**Implemented:**
- Rule matching by app_id, title, instance
- Actions: float, workspace, size, position
- Minimize, maximize, fullscreen states

---

### 4.4 Spring Animations

**Status:** Complete ✓  
**Files:** `src/wm/anim.rs`

**Implemented:**
- Spring physics for window focus/movement
- Configurable tension and friction
- Smooth animations for window transitions

---

### 4.5 Workspace Per-Output Binding

**Status:** Not implemented  
**Files:** `src/wm/mod.rs`

**Problem:** All monitors share active workspace on first monitor. No per-monitor workspace binding.

**Expected Behavior:** Each monitor can show different workspace independently.

**Implementation Notes:**
- Track `active_workspace` per `Monitor` instead of globally
- Update `WlOutput::current_workarea` per monitor
- Handle workspace switch only on focused monitor

---

### 4.6 Preselection Layouts

**Status:** Missing  
**Files:** None

**Expected Behavior:** Pre-select window position before spawning, like dwm's `selstack`.

**Implementation Notes:**
- Track pending geometry for next client
- On spawn, place at pending position instead of layout default
- Clear pending on layout change

---

## 5. Input Handling

### 5.1 Output Hotplug

**Status:** Missing  
**Files:** `src/backend/drm.rs`

**Problem:** No DRM device hotplug monitoring. Monitors detected at startup only.

**Expected Behavior:** Detect monitor connect/disconnect at runtime.

**Implementation Notes:**
- Monitor DRM `drmDevice` events via `drm.rs`
- On hotplug, enumerate connectors, rebuild CRTC assignments
- Notify clients via `wl_output` geometry events
- Update `Axiom::monitors` and `Axiom::outputs`

**Reference:** See `backend/session.rs` for libseat event handling pattern.

---

### 5.2 Touch Support

**Status:** Missing  
**Files:** `src/input/mod.rs`

**Problem:** libinput touch events not handled.

**Expected Behavior:** Touch input for tablets and touchscreens.

**Implementation Notes:**
- Add `TouchState` to track touch points
- Handle `touch_down`, `touch_up`, `touch_motion` events
- Implement `wl_touch` protocol for clients
- Add touch-to-pointer emulation option

---

### 5.3 Gesture Recognition

**Status:** Missing  
**Files:** None

**Expected Behavior:** Pinch-to-zoom, three-finger swipe for workspace switch.

**Implementation Notes:**
- Track touch history for gesture detection
- Implement swipe threshold and velocity calculation
- Bind gestures to Lua functions

---

### 5.4 Tablet Support

**Status:** Missing  
**Files:** None

**Expected Protocols:** `zwp_tablet_v2`

**Expected Behavior:** Pen input, pressure sensitivity, eraser.

---

## 6. Portal Integration

### 6.1 PipeWire Screencast (Stubbed)

**Status:** Portal D-Bus complete, stream creation incomplete  
**Files:** `src/portal/pipewire_stream.rs`, `src/portal/dbus.rs`

**Current State:** Portal D-Bus interface is complete and functional for screenshots. Screencast stream creation still needs work.

**Expected Behavior:** Capture screen for screenshot/screencast via PipeWire.

**Implementation Notes:**
- Requires `libspa` and `libpipewire` development headers
- Create PipeWire stream with screen source
- Implement `wire_plug_add` callback for stream buffer negotiation
- Copy frames to DMA-BUF or shared memory for compositor use
- Handle libspa version compatibility

---

### 6.2 Screen Cast Portal Details

**Status:** D-Bus interface complete ✓  
**Files:** `src/portal/dbus.rs`

**Implemented:**
- D-Bus portal interface for screenshots
- Screenshot request handling via zwp_linux_dmabuf_v1

**Needed:**
- Complete PipeWire stream initialization
- Handle `ScreenCastStream::Start()` response
- Parse `pipewire_remote_fd` from response

---

## 7. Lua API & Scripting

### 7.1 Missing API Functions

**Status:** Incomplete  
**Files:** `src/scripting/lua_api.rs`

**Missing Functions:**
```lua
axiom.group()           -- Group windows
axiom.ungroup()        -- Ungroup windows  
axiom.group_next()     -- Next in group
axiom.group_prev()     -- Previous in group

axiom.snap(corner)     -- Snap to corner
axiom.center()         -- Center window

axiom.minimize()       -- Minimize window (restore missing)
axiom.unminimize()     -- Unminimize window

axiom.swap(direction)   -- Swap with neighbor
axiom.master()          -- Make focused master

axiom.tag(n)            -- Move to tag
axiom.toggle_tag(n)     -- Toggle tag

axiom.get_screen()      -- Get screen geometry
axiom.set_screen()      -- Set screen properties

axiom.screenshot()      -- Take screenshot
axiom.screencast()      -- Start screencast

axiom.restart()         -- Restart compositor
axiom.quit()           -- Quit compositor
```

**Reference:** See existing API pattern in `lua_api.rs` for function registration.

---

### 7.2 AwesomeWM Compatibility Gaps

**Status:** Partial  
**Files:** `src/scripting/lua_api.rs`

**Missing Global Tables:**
```lua
awful.tag              -- Tag manipulation
awful.placement        -- Window positioning
awful.rules            -- Enhanced rule system
awful.menu             -- Application menus
awful.prompt           -- Run prompt
awful.spawn            -- Enhanced spawn with output
```

**Reference:** See `awful` global initialization and extend.

---

### 7.3 Plugin ABI

**Status:** Missing  
**Files:** `src/scripting/abi.rs` (referenced but doesn't exist)

**Problem:** AGENTS.md mentions `src/scripting/abi.rs` but file doesn't exist.

**Expected Behavior:** C ABI for loading external plugins at runtime.

**Implementation Notes:**
- Define C header for plugin interface
- Implement `axiom_plugin_init()` entry point
- Provide Lua API registration callback
- Use `libloading` crate for runtime loading

---

### 7.4 Configuration Schema Validation

**Status:** Missing  
**Files:** None

**Expected Behavior:** Validate config options against schema with helpful error messages.

**Implementation Notes:**
- Define schema for config values
- Validate types and ranges on load
- Show helpful errors for invalid config

---

## 8. IPC Server

### 8.1 Missing IPC Commands

**Status:** Partial  
**Files:** `src/ipc/commands.rs`

**Missing Commands:**
```
getworkspaces      -- List all workspaces with clients
getclients         -- List all clients with geometry
getconfig          -- Dump current config
setlayout          -- Set layout for workspace
setlayoutaxis      -- Set layout with parameters
togglefloating     -- Toggle floating
-focuswindow       -- Focus by window ID
-swapwindow        -- Swap with window ID
-movetoworkspace   -- Move to workspace
```

**Reference:** See existing command pattern in `commands.rs`.

---

### 8.2 WebSocket Support

**Status:** Missing  
**Files:** None

**Expected Behavior:** WebSocket endpoint for browser-based clients.

**Implementation Notes:**
- Add `tokio-tungstenite` for WebSocket
- Same command protocol as Unix socket
- Handle CORS for web clients

---

## 9. Backend & Graphics

### 9.1 Multi-GPU Support

**Status:** Missing  
**Files:** `src/backend/mod.rs`

**Problem:** Only primary GPU used. No GPU switching or multi-adapter rendering.

**Expected Behavior:** Support multiple GPUs, render to any output regardless of GPU.

**Implementation Notes:**
- Enumerate all DRM devices on startup
- Track which output belongs to which GPU
- Create GBM device per GPU
- Handle GPU switching for outputs

---

### 9.2 Render Scheduling / Triple Buffering

**Status:** Basic double-buffering only  
**Files:** `src/backend/gbm.rs`, `src/render/mod.rs`

**Problem:** Simple front/back buffer swap. No adaptive timing.

**Expected Behavior:** Triple buffering for smoother framerates on underpowered devices.

**Implementation Notes:**
- Allocate 3 buffers instead of 2
- Implement explicit sync for proper buffer ordering
- Track frame timing for adaptive scheduling

**Reference:** Hyprland's `new_render_scheduling` implementation.

---

### 9.3 Direct Scanout

**Status:** Missing  
**Files:** None

**Expected Behavior:** Fullscreen windows can scan out directly to display, bypassing compositing.

**Implementation Notes:**
- Check for fullscreen overlay window
- Use DRM `PRIMARY_PLANE` overlay for direct scanout
- Fallback to compositing if not possible

---

### 9.4 Tearing (V-sync Control)

**Status:** Missing  
**Files:** None

**Expected Behavior:** Allow tearing for gaming by disabling compositor sync.

**Implementation Notes:**
- Add `allow_tearing` option
- Set `DRM_MODE_PAGE_FLIP_ASYNC` flag when enabled
- Warn about tearing artifacts with multiple monitors

---

## 10. Code Quality

### 10.1 Compilation Warnings (115 warnings)

**Status:** Needs cleanup  
**Files:** Throughout codebase

**Major Issues:**
1. Duplicate FFI declarations: `libseat_open_device`/`libseat_close_device` in both `src/input/mod.rs` and `src/backend/session.rs`
2. Unused imports: Many files have unused `use` statements
3. Anti-pattern: `drop(reference)` instead of `let _ = reference;` at `src/wm/mod.rs:644`

**Fixes:**
- Consolidate FFI declarations to `src/sys.rs`
- Remove unused imports
- Fix drop pattern

---

### 10.2 Error Handling Inconsistency

**Status:** Needs standardization  
**Files:** Throughout codebase

**Problem:** Mix of `lock().unwrap()` and `if let Ok()` patterns.

**Expected:** Consistent error propagation with `?` operator.

---

### 10.3 Missing `c_void` Import

**Status:** Bug  
**Files:** `src/proto/shm.rs:244`

**Problem:** Uses `c_void` but import only in inner scope.

**Fix:** Add `use std::ffi::c_void;` at file top level.

---

### 10.4 Type Alias Scattering

**Status:** Cleanup  
**Files:** Multiple files

**Problem:** `WindowId = u32` re-exported in multiple places.

**Fix:** Define once in `src/state.rs` and re-export.

---

## 11. Documentation

### 11.1 Missing Documentation

**Files:** Throughout codebase

**Missing Documentation:**
- Module-level docs for `src/backend/`
- API documentation for Lua functions (consider doc comments)
- Architecture decision records for key decisions
- Contributing guidelines

---

## Priority Implementation Order

### Tier 1: Critical (Blocking Production Use)

1. **Window Content Rendering** - Client buffers not appearing (SHM/DMABuf)
2. **Chrome Rendering** - Window decorations only partially visible
3. **Output Hotplug** - Can't connect/disconnect monitors

### Tier 2: High (Core Features)

4. **Window Groups** - Tabbed window management
5. **Blur/Effects** - Visual polish
6. **Minimize/Unminimize** - Basic window state
7. **Fractional Scaling** - HiDPI support

### Tier 3: Medium (Polish)

8. **PipeWire Screencast** - Screen capture
9. **Drag-and-Drop** - File operations
10. **Touch Input** - Tablet support
11. **Virtual Keyboards** - For virtual input

### Tier 4: Low (Enhancement)

12. Window Snapping Guides
13. Gesture Recognition
14. Color Management (HDR)
15. Plugin System
16. Multi-GPU Support
17. Direct Scanout

---

## Quick Reference: File Locations

| Component | File | Lines | Purpose |
|-----------|------|-------|---------|
| Root State | `src/state.rs` | 866 | Surface management, focus, Axiom struct |
| Window Mgmt | `src/wm/mod.rs` | 839 | Windows, workspaces, monitors, layouts |
| Lua API | `src/scripting/lua_api.rs` | 719 | All Lua bindings |
| XDG Shell | `src/proto/xdg_shell.rs` | 511 | Window surface protocols |
| Renderer | `src/render/mod.rs` | 807 | OpenGL compositing |
| Shader Programs | `src/render/programs.rs` | 379 | Shader compilation, VAO/VBO |
| DRM | `src/backend/drm.rs` | 190 | DRM device, CRTC, page flip |
| EGL | `src/backend/egl.rs` | 425 | EGL context, texture creation |
| GBM | `src/backend/gbm.rs` | 61 | Buffer management |
| Layer Shell | `src/proto/layer_shell.rs` | 230 | Waybar, notifications |
| XWayland | `src/xwayland/mod.rs` | 499 | X11 connection manager |
| IPC | `src/ipc/mod.rs` | 324 | Socket commands |
| Seat | `src/proto/seat.rs` | 359 | Keyboard, pointer |
| Status Bar | `src/render/bar.rs` | 401 | Catppuccin bar |
| Input | `src/input/mod.rs` | 565 | libinput handling, keybinds |

---

## Key Patterns

### Event Loop Registration (calloop)
```rust
event_loop.handle().insert_source(
    Generic::new(fd, Interest::READ, Mode::Level),
    |_, _, state| {
        // Handle events
        Ok(PostAction::Continue)
    },
)?;
```

### Surface State Access
```rust
if let Some(surface) = self.surfaces.get(&surface_id) {
    let mut surface = surface.lock().unwrap();
    // Modify surface state
}
```

### Lua Function Registration
```rust
fn axiom_spawn(env: &Lua, cmd: String) -> Result<(), LuaError> {
    let axiom = env.app_data::<RefCell<Axiom>>().unwrap();
    axiom.borrow_mut().spawn(&cmd);
    Ok(())
}
```

### Protocol Global Registration
```rust
display.create_global::<WlCompositor, _>(
    6,
    move |new_client, _| {
        new_client.on_bind(|| ());
    },
);
```

---

*Generated for Axiom compositor development. Last updated: 2026-03-24*
*Major progress: Window/content chrome rendering broken - see Priority Tier 1*
