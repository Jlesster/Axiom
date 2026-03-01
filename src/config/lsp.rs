// lsp.rs — Minimal LSP server for trixie.conf
//
// Speaks the Language Server Protocol over stdin/stdout so editors (neovim,
// VSCode, Helix, …) get diagnostics, hover docs, and completions when editing
// the compositor config.
//
// Run via: `trixie --lsp`  (handled in main.rs before the compositor starts)
//
// Supported capabilities:
//   - textDocument/didOpen, didChange, didClose
//   - textDocument/publishDiagnostics  (parse errors → red squiggles)
//   - textDocument/hover               (key documentation)
//   - textDocument/completion          (top-level keys + values)
//
// No async runtime — uses blocking stdio with a simple read loop.
// All messages are JSON-RPC 2.0 over the LSP base protocol framing
// (Content-Length header + body).

use std::collections::HashMap;
use std::io::{BufRead, BufWriter, Write};

use serde_json::{json, Value};

use crate::config::parser;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Run the LSP server loop. Blocks until the client sends `shutdown` + `exit`.
pub fn run_lsp() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut reader = stdin.lock();

    // Track open documents: uri → text
    let mut docs: HashMap<String, String> = HashMap::new();

    loop {
        let msg = match read_message(&mut reader) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("lsp: read error: {e}");
                break;
            }
        };

        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_owned();

        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method.as_str() {
            "initialize" => {
                let reply = json!({
                    "capabilities": {
                        "textDocumentSync": 1,   // Full sync
                        "hoverProvider": true,
                        "completionProvider": {
                            "triggerCharacters": ["=", " "]
                        }
                    },
                    "serverInfo": {
                        "name": "trixie-lsp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                });
                send_reply(&mut out, id, reply);
            }

            "initialized" => {}

            "shutdown" => {
                send_reply(&mut out, id, Value::Null);
            }

            "exit" => break,

            "textDocument/didOpen" => {
                if let Some(uri) = uri_from_params(&params) {
                    let text = params["textDocument"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    let diags = diagnose(&text, &uri);
                    send_diagnostics(&mut out, &uri, diags);
                    docs.insert(uri, text);
                }
            }

            "textDocument/didChange" => {
                if let Some(uri) = uri_from_params(&params) {
                    let text = params["contentChanges"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned();
                    let diags = diagnose(&text, &uri);
                    send_diagnostics(&mut out, &uri, diags);
                    docs.insert(uri, text);
                }
            }

            "textDocument/didClose" => {
                if let Some(uri) = uri_from_params(&params) {
                    docs.remove(&uri);
                    // Clear diagnostics on close.
                    send_diagnostics(&mut out, &uri, vec![]);
                }
            }

            "textDocument/hover" => {
                let uri = uri_from_params(&params).unwrap_or_default();
                let text = docs.get(&uri).cloned().unwrap_or_default();
                let line = params["position"]["line"].as_u64().unwrap_or(0) as usize;
                let ch = params["position"]["character"].as_u64().unwrap_or(0) as usize;
                let hover = hover_at(&text, line, ch);
                send_reply(&mut out, id, hover);
            }

            "textDocument/completion" => {
                let uri = uri_from_params(&params).unwrap_or_default();
                let text = docs.get(&uri).cloned().unwrap_or_default();
                let line = params["position"]["line"].as_u64().unwrap_or(0) as usize;
                let completions = completions_at(&text, line);
                send_reply(&mut out, id, json!({ "items": completions }));
            }

            _ => {
                // Unknown request — send error if it had an id.
                if id.is_some() {
                    send_error(&mut out, id, -32601, "Method not found");
                }
            }
        }
    }
}

// ── Diagnostics ───────────────────────────────────────────────────────────────

fn diagnose(text: &str, _uri: &str) -> Vec<Value> {
    let result = parser::parse(text);
    result
        .errors
        .iter()
        .map(|e| {
            // Parser gives byte offsets; convert to line/col.
            let (line, col) = byte_to_lc(text, e.span.start);
            let (end_line, end_col) = byte_to_lc(text, e.span.end.max(e.span.start + 1));
            json!({
                "range": {
                    "start": { "line": line, "character": col },
                    "end":   { "line": end_line, "character": end_col }
                },
                "severity": 1,   // Error
                "source": "trixie",
                "message": e.message
            })
        })
        .collect()
}

fn byte_to_lc(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let prefix = &text[..byte];
    let line = prefix.chars().filter(|&c| c == '\n').count();
    let col = prefix.rfind('\n').map(|i| byte - i - 1).unwrap_or(byte);
    (line, col)
}

// ── Hover ─────────────────────────────────────────────────────────────────────

fn hover_at(text: &str, line: usize, _ch: usize) -> Value {
    // Find the key on this line.
    let line_text = text.lines().nth(line).unwrap_or("");
    let key = line_text
        .split('=')
        .next()
        .unwrap_or("")
        .trim()
        .split_whitespace()
        .last()
        .unwrap_or("");

    let Some(doc) = KEY_DOCS.iter().find(|(k, _)| *k == key) else {
        return Value::Null;
    };

    json!({
        "contents": {
            "kind": "markdown",
            "value": doc.1
        }
    })
}

// ── Completion ────────────────────────────────────────────────────────────────

fn completions_at(text: &str, line: usize) -> Vec<Value> {
    let line_text = text.lines().nth(line).unwrap_or("");
    let trimmed = line_text.trim();

    // If we're after a `=`, complete values for the current key.
    if trimmed.contains('=') {
        let key = trimmed.split('=').next().unwrap_or("").trim();
        return value_completions(key);
    }

    // Otherwise complete key names.
    KEY_DOCS
        .iter()
        .map(|(k, doc)| {
            json!({
                "label": k,
                "kind": 10,      // Property
                "detail": "trixie config key",
                "documentation": { "kind": "markdown", "value": doc }
            })
        })
        .collect()
}

fn value_completions(key: &str) -> Vec<Value> {
    let values: &[&str] = match key {
        "layout" => &["bsp", "columns", "rows", "monocle", "threecol"],
        "bar_position" => &["top", "bottom"],
        "border_style" => &["solid", "none"],
        _ => return vec![],
    };
    values
        .iter()
        .map(|v| json!({ "label": v, "kind": 12 }))
        .collect()
}

// ── Key documentation table ───────────────────────────────────────────────────

static KEY_DOCS: &[(&str, &str)] = &[
    ("font",          "Path to the TTF/OTF font used for the bar and chrome UI.\n\nExample: `font = \"/usr/share/fonts/JetBrainsMono.ttf\"`"),
    ("font_size",     "Font size in pixels for the chrome UI.\n\nExample: `font_size = 14px`"),
    ("gap",           "Gap between tiled panes in pixels.\n\nExample: `gap = 8px`"),
    ("border_width",  "Width of the focus border in pixels. Set to 0 to disable.\n\nExample: `border_width = 2px`"),
    ("workspaces",    "Number of workspaces (1–32).\n\nExample: `workspaces = 9`"),
    ("seat_name",     "libseat seat name. Usually `seat0`.\n\nExample: `seat_name = \"seat0\"`"),
    ("bar_position",  "Position of the status bar: `top` or `bottom`.\n\nExample: `bar_position = bottom`"),
    ("bar_height",    "Height of the status bar in pixels.\n\nExample: `bar_height = 24px`"),
    ("layout",        "Default tiling layout: `bsp`, `columns`, `rows`, `monocle`, `threecol`.\n\nExample: `layout = bsp`"),
    ("exec_once",     "Commands to run once on startup.\n\nExample:\n```\nexec_once firefox {}\n```"),
    ("exec",          "Commands to run on every config reload.\n\nExample:\n```\nexec waybar {}\n```"),
    ("bind",          "Keybind: `bind <mods>+<key> = <action> [args]`\n\nExample: `bind super+return = exec alacritty`"),
    ("active_border", "Border colour for the focused window (hex RGBA).\n\nExample: `active_border = #b4befe`"),
    ("inactive_border","Border colour for unfocused windows (hex RGBA).\n\nExample: `inactive_border = #45475a`"),
    ("pane_bg",       "Background fill colour behind panes (hex RGBA).\n\nExample: `pane_bg = #1e1e2e`"),
];

// ── LSP message framing ───────────────────────────────────────────────────────

fn read_message(reader: &mut impl BufRead) -> Result<Value, Box<dyn std::error::Error>> {
    // Read headers until blank line.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some(val) = line.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse()?;
        }
    }

    if content_length == 0 {
        return Err("zero content-length".into());
    }

    let mut buf = vec![0u8; content_length];
    use std::io::Read;
    reader.read_exact(&mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn send_message(out: &mut impl Write, msg: &Value) {
    let body = msg.to_string();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = out.flush();
}

fn send_reply(out: &mut impl Write, id: Option<Value>, result: Value) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
    );
}

fn send_error(out: &mut impl Write, id: Option<Value>, code: i32, msg: &str) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": msg }
        }),
    );
}

fn send_diagnostics(out: &mut impl Write, uri: &str, diagnostics: Vec<Value>) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": diagnostics
            }
        }),
    );
}

fn uri_from_params(params: &Value) -> Option<String> {
    params["textDocument"]["uri"].as_str().map(str::to_owned)
}
