# Trixie - Agent Instructions

## Project Overview
Trixie is a Wayland compositor (twin/twm) built with Smithay that renders its UI chrome (bars, window borders, pane layouts) using trixui - a library that mimics ratatui's styling and rendering approach.

## Final Goal
Build a production-ready Wayland compositor that provides a **ratatui-inspired TUI aesthetic** - all compositor chrome (window decorations, taskbar, workspaces, popups) should look and feel like a terminal UI application built with ratatui.

## Design Principles

### Visual Style
- All UI elements should use monospace fonts and grid-based layouts typical of terminal UIs
- Color schemes should match ratatui's default palette (bold highlights, muted backgrounds)
- Borders should be single or double-line ASCII box-drawing characters
- UI should have crisp, rectangular pixel-aligned rendering

### Architecture
- Use Smithay for Wayland protocol handling
- Use trixui for rendering compositor chrome (bar, borders, popups)
- Implement TWM features: tiling, floating, workspaces, window management

### Key Features
- Window tiling (horizontal, vertical, stacked layouts)
- Workspace management (multiple workspaces, switching)
- Status bar with workspace indicators, window titles, system info
- Keyboard-driven window management (no mouse required for core operations)
- Border rendering for focused/unfocused windows

## Development Guidelines
- All UI code should use trixui primitives (Canvas, Rect, Styled, etc.)
- Follow the existing pattern in `src/chrome/` for rendering chrome
- Configuration in `src/config/` follows Wayland conventions
- Test compositor with standard Wayland clients (foot, wezterm)
