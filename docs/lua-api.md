# Axiom Lua API Reference

**Source of Truth:**
- Primary implementation: `src/scripting/lua_api.rs`
- Global tables/signals: `src/scripting/signals.rs`
- Window manager state: `src/wm/mod.rs`
- Window rules: `src/wm/rules.rs`

---

## Table of Contents

1. [Global Tables](#1-global-tables)
2. [axiom Table](#2-axiom-table)
3. [Client Object](#3-client-object)
4. [Key Combos](#4-key-combos)
5. [Signals](#5-signals)
6. [Color Format](#6-color-format)
7. [Examples](#7-examples)

---

## 1. Global Tables

### `axiom`

Primary namespace for all Axiom functionality. See [Section 2](#2-axiom-table) for full API.

**Source:** `lua_api.rs:83-403` (table creation and all function registrations)

---

### `client`

AwesomeWM-compatible global table for window signals and access.

**Source:** `signals.rs:44-81`

#### Properties

| Property | Type | Source | Description |
|----------|------|--------|-------------|
| `get` | function | `signals.rs:71-79` | Returns all managed clients as a table |

#### Methods

| Method | Signature | Source | Description |
|--------|-----------|--------|-------------|
| `connect_signal` | `(signal, func) -> ()` | `signals.rs:47-59` | Connect callback to a client signal |
| `disconnect_signal` | `(signal) -> ()` | `signals.rs:61-65` | Disconnect all handlers for a signal |
| `get` | `() -> table` | `signals.rs:72-79` | Returns all managed clients |

**Lua Example:**
```lua
client.connect_signal("focus", function(c)
    print("Focused: " .. c.name)
end)

local clients = client.get()
```

---

### `tag`

AwesomeWM-compatible tag (workspace) signal table.

**Source:** `signals.rs:84-103`

#### Methods

| Method | Signature | Source | Description |
|--------|-----------|--------|-------------|
| `connect_signal` | `(signal, func) -> ()` | `signals.rs:86-98` | Connect callback to a tag signal |
| `disconnect_signal` | `(signal) -> ()` | `signals.rs:61-65` | Disconnect all handlers for a signal |

---

### `screen`

Screen/monitor utility table.

**Source:** `signals.rs:106-118`

#### Methods

| Method | Signature | Source | Description |
|--------|-----------|--------|-------------|
| `count` | `() -> integer` | `signals.rs:108-116` | Returns number of monitors |

**Lua Example:**
```lua
local num_screens = screen.count()
```

---

## 2. `axiom` Table

All functions live on the global `axiom` table, registered in `lua_api.rs:81-404`.

---

### Configuration

#### `axiom.set(t)`

**Source:** `lua_api.rs:89-123`

Configure window manager settings. All fields are optional.

```lua
axiom.set {
    border_width = 2,           -- Border width in pixels (stored in wm.config.border_w)
    gap = 6,                   -- Gap between windows (wm.config.gap)
    bar_height = 24,           -- Status bar height in pixels (wm.config.bar_height)
    bar_at_bottom = false,     -- Position bar at bottom (wm.config.bar_at_bottom)
    workspaces = 9,             -- Number of workspaces (wm.config.workspaces_count)
    border_active = "#b4ccff", -- Active window border (hex color)
    border_inactive = "#454757",-- Inactive window border (hex color)
    bar_bg = "#181822",        -- Status bar background (hex color)
}
```

**Color Format:** Hex strings with `#` prefix. Accepts 6-digit (`#RRGGBB`) or 8-digit (`#RRGGBBAA`). See `lua_api.rs:701-719` for parsing logic.

**Implementation Detail:** Uses unsafe pointer access to `wm.config` (`lua_api.rs:91-95`). Colors are parsed via `parse_color()` at `lua_api.rs:701-719`.

---

### Process Control

#### `axiom.spawn(cmd)`

**Source:** `lua_api.rs:125-136`

Execute a shell command asynchronously via `sh -c`.

```lua
axiom.spawn("alacritty")
axiom.spawn("firefox --new-window")
axiom.spawn("sh -c 'echo hello | dmenu'")
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `cmd` | string | `lua_api.rs:128` | Shell command to execute |

**Implementation:** Uses `std::process::Command::new("sh").arg("-c").arg(&cmd).spawn()` at `lua_api.rs:129-133`.

---

#### `axiom.notify(msg, ms?)`

**Source:** `lua_api.rs:138-148`

Send a desktop notification using `notify-send`.

```lua
axiom.notify("Hello, World!")
axiom.notify("Important message", 5000)  -- Show for 5 seconds
```

**Parameters:**
| Name | Type | Default | Source | Description |
|------|------|---------|--------|-------------|
| `msg` | string | — | `lua_api.rs:141` | Notification message |
| `ms` | integer | 3000 | `lua_api.rs:141` | Duration in milliseconds |

**Implementation:** Calls `notify-send -t <ms> Axiom <msg>` at `lua_api.rs:142-144`.

---

### Keybinds

Keybinds are stored in Lua registry table `axiom_keybinds` (`lua_api.rs:151`).

#### `axiom.key(combo, func)` / `axiom.bind(combo, func)`

**Source:** `lua_api.rs:153-159`

Register a keybind. Callback is invoked when key combo is pressed.

```lua
axiom.key("Super+Return", function()
    axiom.spawn("alacritty")
end)

-- Alternative alias
axiom.bind("Super+Shift+q", function()
    axiom.close()
end)
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `combo` | string | `lua_api.rs:155` | Key combination (see [Key Combos](#4-key-combos)) |
| `func` | function | `lua_api.rs:155` | Function to execute on keypress |

**Implementation:** Normalizes combo via `normalise_combo()` (`lua_api.rs:688-699`) and stores in registry table.

---

#### `axiom.unkey(combo)` / `axiom.unbind(combo)`

**Source:** `lua_api.rs:162-169`

Remove a registered keybind.

```lua
axiom.unkey("Super+Return")
axiom.unbind("Super+p")
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `combo` | string | `lua_api.rs:164` | Key combination to remove |

**Implementation:** Sets the registry entry to `Nil` at `lua_api.rs:166`.

---

### Workspace Management

#### `axiom.workspace(n)` / `axiom.goto(n)`

**Source:** `lua_api.rs:171-183`

Switch to workspace n (1-indexed internally converted to 0-indexed).

```lua
axiom.workspace(2)
axiom.goto(3)
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `n` | integer | `lua_api.rs:176` | Workspace index (1-indexed) |

**Implementation:** Queues `LuaAction::SwitchWorkspace(n.saturating_sub(1))` at `lua_api.rs:177-180`. Action processed in `apply()` at `lua_api.rs:550-553`.

**Note:** Uses `saturating_sub(1)` to handle 0 gracefully (line 179).

---

#### `axiom.ws()` / `axiom.active_workspace()`

**Source:** `lua_api.rs:202-209`

Returns the current workspace index (1-based).

```lua
local current = axiom.ws()
print("On workspace: " .. current)
```

**Returns:** integer (1-indexed)

**Implementation:** Reads `wm.active_ws() + 1` at `lua_api.rs:207`. `active_ws()` is defined in `wm/mod.rs`.

---

#### `axiom.send(n)` / `axiom.move_to_workspace(n)`

**Source:** `lua_api.rs:185-200`

Move the focused window to workspace n.

```lua
axiom.send(2)
axiom.move_to_workspace(3)
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `n` | integer | `lua_api.rs:190` | Target workspace index (1-indexed) |

**Implementation:** Gets focused window via `wm.focused_window()` (`lua_api.rs:192`), queues `LuaAction::MoveToWorkspace(id, n-1)` at `lua_api.rs:194-196`.

---

#### `axiom.layout(ws, name)`

**Source:** `lua_api.rs:211-224`

Set the tiling layout for a workspace.

```lua
axiom.layout(1, "tile")       -- MasterStack layout
axiom.layout(2, "bsp")       -- Binary space partition
axiom.layout(3, "monocle")   -- Fullscreen one window
axiom.layout(4, "float")     -- Free positioning
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `ws` | integer | `lua_api.rs:216` | Workspace index (1-indexed) |
| `name` | string | `lua_api.rs:216` | Layout name |

**Valid Layout Names** (from `lua_api.rs:676-686`):
| Name | Variant Aliases | Layout Enum |
|------|----------------|-------------|
| `"tile"` | `"master_stack"` | `Layout::MasterStack` |
| `"bsp"` | — | `Layout::Bsp` |
| `"monocle"` | `"max"` | `Layout::Monocle` |
| `"float"` | — | `Layout::Float` |

**Error:** Throws `LuaError::RuntimeError` for unknown layouts (`lua_api.rs:682-684`).

---

### Window Focus & Navigation

#### `axiom.focus(dir)`

**Source:** `lua_api.rs:230-237`

Focus window in specified direction relative to focused window.

```lua
axiom.focus("left")
axiom.focus("right")
axiom.focus("up")
axiom.focus("down")
```

**Parameters:**
| Name | Type | Source | Valid Values |
|------|------|--------|--------------|
| `dir` | string | `lua_api.rs:232` | `"left"`, `"right"`, `"up"`, `"down"` |

**Implementation:** Pushes direction to `axiom_pending_focus_dir` table (`lua_api.rs:233-234`). Processed in `drain_dir_actions()` at `lua_api.rs:467-498`, which calls `wm.focus_direction(0-3)` at lines 470-483.

**Direction Mapping** (`lua_api.rs:469-484`):
| Direction | Index | Method |
|-----------|-------|--------|
| `"left"` | 0 | `focus_direction(0)` |
| `"right"` | 1 | `focus_direction(1)` |
| `"up"` | 2 | `focus_direction(2)` |
| `"down"` | 3 | `focus_direction(3)` |

---

#### `axiom.cycle(delta)`

**Source:** `lua_api.rs:239-246`

Cycle focus through windows in the active workspace.

```lua
axiom.cycle(1)   -- Cycle forward
axiom.cycle(-1)  -- Cycle backward
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `delta` | integer | `lua_api.rs:241` | Direction to cycle (positive = forward, negative = backward) |

**Implementation:** Pushes `"cycle+"` or `"cycle-"` to `axiom_pending_focus_dir` (`lua_api.rs:243-244`). Processed in `drain_dir_actions()` at `lua_api.rs:485-494`, calling `workspace.cycle_focus(delta)` from `wm/mod.rs:126-133`.

---

#### `axiom.move(dir)`

**Source:** `lua_api.rs:248-255`

Move the focused window in the specified direction.

```lua
axiom.move("left")
axiom.move("right")
axiom.move("up")
axiom.move("down")
```

**Parameters:**
| Name | Type | Source | Valid Values |
|------|------|--------|--------------|
| `dir` | string | `lua_api.rs:250` | `"left"`, `"right"`, `"up"`, `"down"` |

**Implementation:** Pushes direction to `axiom_pending_move_dir` table (`lua_api.rs:251-252`). Processed in `drain_dir_actions()` at `lua_api.rs:501-512`, calling `wm.move_direction(0-3)` at `lua_api.rs:510`.

**Direction Mapping** (`lua_api.rs:503-508`):
| Direction | Index |
|-----------|-------|
| `"left"` | 0 |
| `"right"` | 1 |
| `"up"` | 2 |
| `"down"` | 3 |

---

### Focused Window Actions

All these operate on the currently focused window.

#### `axiom.close()`

**Source:** `lua_api.rs:258-269`

Close the focused window.

```lua
axiom.close()
```

**Implementation:** Gets focused window via `wm.focused_window()` (`lua_api.rs:263-264`), queues `LuaAction::CloseId(id)` at `lua_api.rs:265`. Action processed in `apply()` at `lua_api.rs:543-545`, calling `state.close_window(id)`.

---

#### `axiom.float()`

**Source:** `lua_api.rs:285-298`

Toggle floating state of the focused window.

```lua
axiom.float()
```

**Implementation:** Gets focused window, toggles current state (`lua_api.rs:291-293`), queues `LuaAction::SetFloat(id, on)`. Action processed in `apply()` at `lua_api.rs:561-567`, setting `w.floating = on` and calling `wm.reflow()`.

---

#### `axiom.fullscreen()`

**Source:** `lua_api.rs:271-284`

Toggle fullscreen state of the focused window.

```lua
axiom.fullscreen()
```

**Implementation:** Gets focused window, toggles current state (`lua_api.rs:277-279`), queues `LuaAction::SetFullscreen(id, on)`. Action processed in `apply()` at `lua_api.rs:568-572`, calling `wm.fullscreen_window(id, on)` and `state.send_configure_focused()`.

---

### Layout Controls

#### `axiom.inc_master()`

**Source:** `lua_api.rs:300`

Increment the number of master windows (for MasterStack layout).

```lua
axiom.inc_master()
```

**Implementation:** Queues `LuaAction::IncMaster` (macro at `lua_api.rs:69-77`). Action processed in `apply()` at `lua_api.rs:576-580`, calling `wm.inc_master()` then `wm.reflow()`.

---

#### `axiom.dec_master()`

**Source:** `lua_api.rs:301`

Decrement the number of master windows.

```lua
axiom.dec_master()
```

**Implementation:** Queues `LuaAction::DecMaster`. Action processed in `apply()` at `lua_api.rs:581-585`.

---

### Window Access

#### `axiom.clients()`

**Source:** `lua_api.rs:305-326`

Returns array of all managed client windows across all workspaces.

```lua
for _, c in ipairs(axiom.clients()) do
    print(c.id, c.app_id, c.title)
end
```

**Returns:** Table array of [Client Objects](#3-client-object)

**Implementation:** Iterates `wm.workspaces` and `ws.windows` (`lua_api.rs:314-315`), building client tables via `build_client()` at `lua_api.rs:599-668`.

**Note:** Uses `HashSet` to avoid duplicates when windows appear in multiple places (`lua_api.rs:313`).

---

#### `axiom.focused()`

**Source:** `lua_api.rs:328-341`

Returns the currently focused client, or `nil`.

```lua
local c = axiom.focused()
if c then
    print("Focused: " .. c.title)
end
```

**Returns:** [Client Object](#3-client-object) or `nil`

**Implementation:** Gets focused window from `wm.focused_window()` (`lua_api.rs:335`), returns client table built via `build_client()`.

---

### Monitor/Screen Access

#### `axiom.screens()`

**Source:** `lua_api.rs:343-361`

Returns array of monitor/display objects.

```lua
for i, s in ipairs(axiom.screens()) do
    print("Monitor " .. i .. ": " .. s.width .. "x" .. s.height)
end
```

**Returns:** Table array of monitor objects

**Monitor Object Properties** (from `lua_api.rs:350-356`):
| Property | Type | Source | Description |
|----------|------|--------|-------------|
| `index` | integer | `lua_api.rs:351` | Monitor index (1-indexed) |
| `width` | integer | `lua_api.rs:352` | Width in pixels |
| `height` | integer | `lua_api.rs:353` | Height in pixels |
| `x` | integer | `lua_api.rs:354` | X position |
| `y` | integer | `lua_api.rs:355` | Y position |
| `workspace` | integer | `lua_api.rs:356` | Active workspace index (1-indexed) |

---

### Window Rules

#### `axiom.rule(rule)`

**Source:** `lua_api.rs:363-372`

Define a rule to apply properties to matching windows. Rules are evaluated when windows open.

```lua
axiom.rule {
    match = { app_id = "firefox" },
    action = { workspace = 2 }
}

axiom.rule {
    match = { title = "Picture-in-Picture" },
    action = { float = true }
}
```

**Parameters:**
| Field | Type | Source | Description |
|-------|------|--------|-------------|
| `match` | table | `lua_api.rs:367` | Matching criteria |
| `action` | table | `lua_api.rs:367` | Effects to apply |

**Match Criteria** (from `wm/rules.rs:10-15`):
| Field | Description |
|-------|-------------|
| `app_id` | Match by application ID |
| `title` | Match by window title |
| `{app_id, title}` | Match both |

**Rule Effects** (from `wm/rules.rs:32-42`):
| Effect | Type | Description |
|--------|------|-------------|
| `float = true` | bool | Float the window |
| `workspace = n` | integer | Move to workspace n |
| `size = {w, h}` | table | Set window size |
| `position = {x, y}` | table | Set window position |
| `opacity = n` | float | Set opacity (0.0-1.0) |
| `sticky = true` | bool | Make sticky |
| `no_decoration = true` | bool | Remove decorations |
| `scratchpad = "name"` | string | Add to scratchpad |
| `inhibit_idle = true` | bool | Inhibit idle timeout |

**Implementation:** Rules stored in `axiom_rules` registry table (`lua_api.rs:364`), later parsed and added to `wm.config.rules` in `scripting/mod.rs`.

---

### Signals

#### `axiom.on(event, func)`

**Source:** `lua_api.rs:377-392`

Connect a callback to a signal. Multiple callbacks can be connected.

```lua
axiom.on("compositor.ready", function()
    axiom.notify("Axiom is ready!")
end)

axiom.on("client.focus", function(c)
    print("Focused: " .. c.title)
end)
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `event` | string | `lua_api.rs:379` | Signal name |
| `func` | function | `lua_api.rs:379` | Callback function |

**Signals Available:** See [Section 5](#5-signals).

**Implementation:** Stores in `axiom_signals` registry table (`lua_api.rs:375,380-390`).

---

#### `axiom.off(event)`

**Source:** `lua_api.rs:394-401`

Disconnect all handlers for a signal.

```lua
axiom.off("client.focus")
```

**Parameters:**
| Name | Type | Source | Description |
|------|------|--------|-------------|
| `event` | string | `lua_api.rs:396` | Signal name |

**Implementation:** Sets registry entry to `Nil` at `lua_api.rs:398`.

---

### Compositor Control

#### `axiom.reload()`

**Source:** `lua_api.rs:302`

Reload the configuration file and reapply rules.

```lua
axiom.reload()
```

**Implementation:** Queues `LuaAction::Reload`. Action processed in `apply()` at `lua_api.rs:586-588`, calling `state.reload_config()`.

---

#### `axiom.quit()`

**Source:** `lua_api.rs:303`

Quit the compositor.

```lua
axiom.quit()
```

**Implementation:** Queues `LuaAction::Quit`. Action processed in `apply()` at `lua_api.rs:589-593`, setting `state.running.store(false)`.

---

## 3. Client Object

Returned by `axiom.clients()`, `axiom.focused()`, and signal callbacks.

Created by `build_client()` function at `lua_api.rs:599-668`.

### Properties

| Property | Type | Source | Description |
|----------|------|--------|-------------|
| `id` | integer | `lua_api.rs:608` | Unique window identifier |
| `app_id` | string | `lua_api.rs:609` | Application identifier |
| `title` | string | `lua_api.rs:610` | Window title |
| `floating` | boolean | `lua_api.rs:611` | Is floating |
| `fullscreen` | boolean | `lua_api.rs:612` | Is fullscreen |
| `maximized` | boolean | `lua_api.rs:613` | Is maximized |
| `x` | integer | `lua_api.rs:614` | X position |
| `y` | integer | `lua_api.rs:615` | Y position |
| `width` | integer | `lua_api.rs:616` | Width |
| `height` | integer | `lua_api.rs:617` | Height |

**Source of data:** `win.id`, `win.app_id`, `win.title`, `win.floating`, etc. from `crate::wm::Window` struct (`wm/mod.rs:58-70`).

### Methods

All methods queue actions that are processed at the end of each frame.

#### `c:close()`

**Source:** `lua_api.rs:621-627`

Close this client.

```lua
axiom.focused():close()
```

**Implementation:** Queues `LuaAction::CloseId(id)` at `lua_api.rs:624`.

---

#### `c:focus()`

**Source:** `lua_api.rs:629-636`

Focus this client.

```lua
axiom.clients()[1]:focus()
```

**Implementation:** Queues `LuaAction::FocusId(id)` at `lua_api.rs:633`. Action processed in `apply()` at `lua_api.rs:539-542`, calling `wm.focus_window(id)` and `sync_keyboard_focus()`.

---

#### `c:set_fullscreen(on)`

**Source:** `lua_api.rs:638-645`

Set fullscreen state.

```lua
client:set_fullscreen(true)
client:set_fullscreen(false)
```

**Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `on` | boolean | Fullscreen state |

**Implementation:** Queues `LuaAction::SetFullscreen(id, on)` at `lua_api.rs:642`.

---

#### `c:set_float(on)`

**Source:** `lua_api.rs:647-654`

Set floating state.

```lua
client:set_float(true)
```

**Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `on` | boolean | Floating state |

**Implementation:** Queues `LuaAction::SetFloat(id, on)` at `lua_api.rs:651`.

---

#### `c:move_to(ws)`

**Source:** `lua_api.rs:656-665`

Move client to a different workspace.

```lua
axiom.focused():move_to(2)  -- Move to workspace 2
```

**Parameters:**
| Name | Type | Description |
|------|------|-------------|
| `ws` | integer | Target workspace index (1-indexed) |

**Implementation:** Queues `LuaAction::MoveToWorkspace(id, ws.saturating_sub(1))` at `lua_api.rs:660-662`.

---

## 4. Key Combos

Key combos are specified as strings with modifiers joined by `+`.

**Normalization function:** `normalise_combo()` at `lua_api.rs:688-699`

### Modifier Keys

| Input Alias | Canonical Form | Source |
|-------------|----------------|--------|
| `Super`, `Mod4`, `Logo` | `super` | `lua_api.rs:691` |
| `Alt`, `Mod1` | `alt` | `lua_api.rs:692` |
| `Ctrl`, `Control` | `ctrl` | `lua_api.rs:693` |
| `Shift` | `shift` | `lua_api.rs:694` |

### Examples

```lua
-- Single key
axiom.key("Return", fn)

-- With modifiers
axiom.key("Super+Return", fn)
axiom.key("Super+Shift+q", fn)
axiom.key("Ctrl+Alt+Delete", fn)

-- Case-insensitive (all normalized to lowercase)
axiom.key("SUPER+RETURN", fn)  -- becomes "super+return"
axiom.key("mod4+space", fn)    -- becomes "super+space"
```

### Special Keys

| Key | Description |
|-----|-------------|
| `Return` | Enter/Return key |
| `Tab` | Tab key |
| `Escape` | Escape key |
| `Space` | Spacebar |
| `Print` | Print Screen |
| `Delete` | Delete key |
| `Up`, `Down`, `Left`, `Right` | Arrow keys |
| `F1` - `F12` | Function keys |

---

## 5. Signals

Signals are emitted at various points during compositor operation.

### Signal Constants

Defined in `signals.rs:8-16`:

| Constant | Value | Source | Description |
|----------|-------|--------|-------------|
| `SIG_MANAGE` | `"manage"` | `signals.rs:8` | Window opened |
| `SIG_UNMANAGE` | `"unmanage"` | `signals.rs:9` | Window closed |
| `SIG_FOCUS` | `"focus"` | `signals.rs:10` | Window gained focus |
| `SIG_UNFOCUS` | `"unfocus"` | `signals.rs:11` | Window lost focus |
| `SIG_PROP_TITLE` | `"property::title"` | `signals.rs:12` | Title changed |
| `SIG_PROP_FLOATING` | `"property::floating"` | `signals.rs:13` | Float state changed |
| `SIG_PROP_FULLSCREEN` | `"property::fullscreen"` | `signals.rs:14` | Fullscreen changed |
| `SIG_PROP_URGENT` | `"property::urgent"` | `signals.rs:15` | Urgent state changed |
| `SIG_TAG_SELECTED` | `"property::selected"` | `signals.rs:16` | Tag/workspace selected |

### Emitting Signals

**axiom.on() signals:**
```lua
axiom.on("manage", function(c) end)           -- Window opened
axiom.on("unmanage", function(c) end)        -- Window closed
axiom.on("focus", function(c) end)           -- Window focused
axiom.on("unfocus", function(c) end)         -- Window unfocused
axiom.on("property::title", function(c) end) -- Title changed
axiom.on("property::floating", function(c) end) -- Float changed
axiom.on("property::fullscreen", function(c) end) -- Fullscreen changed
axiom.on("property::selected", function(c) end) -- Workspace changed
```

**client.connect_signal() aliases:**
```lua
client.connect_signal("manage", fn)           -- Same as "client.open"
client.connect_signal("unmanage", fn)        -- Same as "client.close"
client.connect_signal("focus", fn)           -- Same as "client.focus"
client.connect_signal("unfocus", fn)        -- Same as "client.unfocus"
client.connect_signal("property::title", fn)  -- Same as "client.title"
client.connect_signal("property::floating", fn) -- Same as "client.float"
client.connect_signal("property::fullscreen", fn) -- Same as "client.fullscreen"
```

### Client Table in Signals

When signals are emitted, callbacks receive a client table built by `client_to_lua()` (`signals.rs:22-39`):

| Property | Source | Description |
|----------|--------|-------------|
| `id` | `signals.rs:24` | Window ID |
| `app_id` | `signals.rs:25` | Application ID |
| `class` | `signals.rs:26` | Alias for app_id |
| `instance` | `signals.rs:27` | Alias for app_id |
| `name` | `signals.rs:28` | Window title |
| `title` | `signals.rs:29` | Window title |
| `floating` | `signals.rs:30` | Is floating |
| `fullscreen` | `signals.rs:31` | Is fullscreen |
| `maximized` | `signals.rs:32` | Is maximized |
| `x`, `y`, `width`, `height` | `signals.rs:33-36` | Geometry |
| `tags` | `signals.rs:37` | Empty table |

---

## 6. Color Format

Colors are specified as hex strings with `#` prefix.

**Parsing function:** `parse_color()` at `lua_api.rs:701-719`

### Formats

| Format | Example | RGBA Values |
|--------|---------|-------------|
| 6-digit RGB | `#ff0000` | R=1.0, G=0.0, B=0.0, A=1.0 |
| 8-digit RGBA | `#ff000080` | R=1.0, G=0.0, B=0.0, A=0.5 |

### Implementation

```rust
// lua_api.rs:701-719
pub fn parse_color(s: &str) -> Option<[f32; 4]> {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(match s.len() {
        6 => [
            ((v >> 16) & 0xff) as f32 / 255.0,  // R
            ((v >> 8) & 0xff) as f32 / 255.0,   // G
            (v & 0xff) as f32 / 255.0,          // B
            1.0,                                  // A
        ],
        8 => [
            ((v >> 24) & 0xff) as f32 / 255.0,  // R
            ((v >> 16) & 0xff) as f32 / 255.0,  // G
            ((v >> 8) & 0xff) as f32 / 255.0,   // B
            (v & 0xff) as f32 / 255.0,          // A
        ],
        _ => return None,
    })
}
```

---

## 7. Examples

### Complete Configuration

```lua
-- ~/.config/axiom/axiom.rc.lua

-- Configure appearance
axiom.set({
    border_width = 2,
    gap = 6,
    bar_height = 24,
    border_active = "#b4ccff",
    border_inactive = "#454757",
    bar_bg = "#181822",
})

-- Spawn terminal
axiom.key("Super+Return", function()
    axiom.spawn("alacritty")
end)

-- Application launcher
axiom.key("Super+p", function()
    axiom.spawn("dmenu_path | dmenu | xargs sh -c")
end)

-- Window navigation (vim-style)
axiom.key("Super+j", function() axiom.focus("down") end)
axiom.key("Super+k", function() axiom.focus("up") end)
axiom.key("Super+h", function() axiom.focus("left") end)
axiom.key("Super+l", function() axiom.focus("right") end)

-- Window movement
axiom.key("Super+Shift+j", function() axiom.move("down") end)
axiom.key("Super+Shift+k", function() axiom.move("up") end)
axiom.key("Super+Shift+h", function() axiom.move("left") end)
axiom.key("Super+Shift+l", function() axiom.move("right") end)

-- Layout controls
axiom.key("Super+i", function() axiom.inc_master() end)
axiom.key("Super+d", function() axiom.dec_master() end)

-- Workspace switching
axiom.key("Super+1", function() axiom.workspace(1) end)
axiom.key("Super+2", function() axiom.workspace(2) end)
axiom.key("Super+3", function() axiom.workspace(3) end)
axiom.key("Super+4", function() axiom.workspace(4) end)
axiom.key("Super+5", function() axiom.workspace(5) end)

-- Cycle focus
axiom.key("Super+Tab", function() axiom.cycle(1) end)
axiom.key("Super+Shift+Tab", function() axiom.cycle(-1) end)

-- Layout switching
axiom.key("Super+space", function() axiom.layout(axiom.ws(), "bsp") end)

-- Window actions
axiom.key("Super+Shift+q", function() axiom.close() end)
axiom.key("Super+f", function() axiom.fullscreen() end)
axiom.key("Super+Shift+space", function() axiom.float() end)

-- Window rules
axiom.rule({
    match = { app_id = "firefox" },
    action = { workspace = 2 }
})
axiom.rule({
    match = { app_id = "thunderbird" },
    action = { workspace = 3 }
})
axiom.rule({
    match = { title = "Picture-in-Picture" },
    action = { float = true }
})

-- Signals
axiom.on("compositor.ready", function()
    axiom.notify("Axiom started", 3000)
end)

axiom.on("focus", function(c)
    print("Focused: " .. c.title)
end)

-- Compositor control
axiom.key("Super+Shift+r", function() axiom.reload() end)
axiom.key("Super+Shift+e", function() axiom.quit() end)
```

### Floating Terminal

```lua
axiom.key("Super+t", function()
    axiom.spawn("alacritty --class=floating")
end)

axiom.rule({
    match = { app_id = "floating" },
    action = { float = true }
})
```

### Multi-Monitor Setup

```lua
-- List all monitors
for i, s in ipairs(axiom.screens()) do
    print("Monitor " .. i .. ": " .. s.width .. "x" .. s.height .. " at " .. s.x .. "," .. s.y)
end

-- Move focused window to another workspace
axiom.key("Super+Shift+m", function()
    local c = axiom.focused()
    if c then
        c:move_to(2)
    end
end)
```

### Focus Management

```lua
-- Get all clients
local clients = axiom.clients()
for _, c in ipairs(clients) do
    print(c.app_id .. ": " .. c.title)
end

-- Focus specific client
axiom.key("Super+a", function()
    local c = axiom.focused()
    if c then
        c:focus()
    end
end)
```

---

## Appendix: Action Queue System

Actions in the Lua API are queued and processed at the end of each frame. This ensures thread-safety and atomicity.

**Queue type:** `ActionQueue = Arc<Mutex<Vec<LuaAction>>>` (`lua_api.rs:65`)

**LuaAction enum** (`lua_api.rs:48-63`):
```rust
pub enum LuaAction {
    Spawn(String),                  // Spawn command
    FocusId(WindowId),              // Focus window
    CloseId(WindowId),              // Close window
    MoveToWorkspace(WindowId, usize), // Move window
    SwitchWorkspace(usize),         // Switch workspace
    SetLayout(usize, Layout),       // Set layout
    SetFloat(WindowId, bool),       // Set float
    SetFullscreen(WindowId, bool),  // Set fullscreen
    SetWindowTitle(WindowId, String), // Set title
    IncMaster,                      // Inc master count
    DecMaster,                      // Dec master count
    Reload,                         // Reload config
    Quit,                           // Quit compositor
}
```

**Drain functions:**
- `drain(queue, state)` — processes queued actions (`lua_api.rs:449-455`)
- `drain_actions(actions, state)` — processes given actions (`lua_api.rs:458-462`)
- `drain_dir_actions(state)` — processes pending focus/move directions (`lua_api.rs:464-513`)

---

*Last updated: 2026-03-23*
*Source: `src/scripting/lua_api.rs`, `src/scripting/signals.rs`, `src/wm/mod.rs`, `src/wm/rules.rs`*
