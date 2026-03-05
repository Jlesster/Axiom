// chrome/custom.rs — Background-polled cache for custom bar modules.
//
// Each named custom module gets one entry. A background thread re-runs the
// shell command at the configured interval and writes the result into the
// shared cache. ChromeApp::view reads from the cache without blocking.
//
// Usage in chrome/mod.rs:
//   state.custom_cache.get("my_module")   →  Option<String>

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ── Per-module state ──────────────────────────────────────────────────────────

struct Entry {
    command: String,
    interval: Duration,
    last_poll: Option<Instant>,
    text: Arc<Mutex<String>>,
}

// ── Cache ─────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct CustomModuleCache {
    entries: HashMap<String, Entry>,
}

impl CustomModuleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a module. Idempotent — re-registering the same name is a no-op.
    pub fn register(&mut self, name: &str, command: &str, interval_ms: u64) {
        if self.entries.contains_key(name) {
            return;
        }
        self.entries.insert(
            name.to_string(),
            Entry {
                command: command.to_string(),
                interval: Duration::from_millis(interval_ms),
                last_poll: None,
                text: Arc::new(Mutex::new(String::new())),
            },
        );
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        for entry in self.entries.values_mut() {
            let stale = entry
                .last_poll
                .map(|t| now.duration_since(t) >= entry.interval)
                .unwrap_or(true);

            if stale {
                entry.last_poll = Some(now);
                let cmd = entry.command.clone();
                let out = Arc::clone(&entry.text);
                std::thread::spawn(move || {
                    if let Ok(result) = std::process::Command::new("sh").args(["-c", &cmd]).output()
                    {
                        let text = String::from_utf8_lossy(&result.stdout).trim().to_string();
                        if let Ok(mut guard) = out.lock() {
                            *guard = text;
                        }
                    }
                });
            }
        }
    }

    /// Read the current cached output for a named module.
    pub fn get(&self, name: &str) -> Option<String> {
        let entry = self.entries.get(name)?;
        entry.text.lock().ok().map(|g| g.clone())
    }
}
