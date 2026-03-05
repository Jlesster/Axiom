// chrome/custom.rs — Custom bar module cache with background polling.
//
// Shell-command bar modules (like waybar's `custom/`) run their command
// in a background thread at a configured interval.  The latest output is
// stored here and read on the render thread each frame.
//
// Usage:
//   1. Call `cache.register(name, cmd, args, interval_secs)` at startup.
//   2. Call `cache.start_all()` to launch the polling threads.
//   3. Call `cache.get(name)` in `build_zone()` to read the latest output.
//   4. Call `cache.stop_all()` on compositor exit.
//
// Thread safety: `Arc<Mutex<String>>` per entry, so reads from the render
// thread never block the background worker and vice-versa.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

// ── Entry ─────────────────────────────────────────────────────────────────────

struct ModuleEntry {
    /// Latest stdout output, trimmed.
    output: Arc<Mutex<String>>,
    /// Stop flag — set to true to terminate the polling thread.
    stop: Arc<Mutex<bool>>,
    /// Poll interval in seconds.
    interval: u64,
    /// Command and arguments.
    cmd: String,
    args: Vec<String>,
}

// ── Cache ─────────────────────────────────────────────────────────────────────

/// Cache for custom bar module command output.
#[derive(Default)]
pub struct CustomModuleCache {
    entries: HashMap<String, ModuleEntry>,
}

impl CustomModuleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a shell command module.
    ///
    /// * `name`          — the module name (used as the key in `bar_modules`).
    /// * `cmd`           — the binary to execute.
    /// * `args`          — arguments.
    /// * `interval_secs` — how often to re-run the command (minimum 1).
    pub fn register(
        &mut self,
        name: impl Into<String>,
        cmd: impl Into<String>,
        args: Vec<String>,
        interval_secs: u64,
    ) {
        let name = name.into();
        let entry = ModuleEntry {
            output: Arc::new(Mutex::new(String::new())),
            stop: Arc::new(Mutex::new(false)),
            interval: interval_secs.max(1),
            cmd: cmd.into(),
            args,
        };
        self.entries.insert(name, entry);
    }

    /// Register a one-liner shell command (run via `sh -c`).
    /// Convenient for config-driven `exec = "…"` style modules.
    pub fn register_shell(
        &mut self,
        name: impl Into<String>,
        shell_cmd: impl Into<String>,
        interval_secs: u64,
    ) {
        let cmd = shell_cmd.into();
        self.register(name, "sh", vec!["-c".into(), cmd], interval_secs);
    }

    /// Start all registered polling threads.  Safe to call multiple times
    /// (threads that are already running are skipped).
    pub fn start_all(&self) {
        for (name, entry) in &self.entries {
            let output = Arc::clone(&entry.output);
            let stop = Arc::clone(&entry.stop);
            let interval = Duration::from_secs(entry.interval);
            let cmd = entry.cmd.clone();
            let args = entry.args.clone();
            let name = name.clone();

            thread::Builder::new()
                .name(format!("bar-module:{}", name))
                .spawn(move || {
                    loop {
                        // Run the command.
                        match std::process::Command::new(&cmd).args(&args).output() {
                            Ok(out) => {
                                let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                                if let Ok(mut guard) = output.lock() {
                                    *guard = text;
                                }
                            }
                            Err(e) => {
                                tracing::warn!("bar module '{name}': command failed: {e}");
                            }
                        }

                        // Sleep in small increments so stop is checked promptly.
                        let steps = (interval.as_millis() / 100).max(1) as u64;
                        for _ in 0..steps {
                            thread::sleep(Duration::from_millis(100));
                            if stop.lock().map(|g| *g).unwrap_or(false) {
                                return;
                            }
                        }
                    }
                })
                .ok();
        }
    }

    /// Stop all polling threads gracefully.
    pub fn stop_all(&self) {
        for entry in self.entries.values() {
            if let Ok(mut stop) = entry.stop.lock() {
                *stop = true;
            }
        }
    }

    /// Read the latest output for `name`, or `None` if not registered / not yet run.
    pub fn get(&self, name: &str) -> Option<String> {
        let entry = self.entries.get(name)?;
        entry
            .output
            .lock()
            .ok()
            .map(|g| g.clone())
            .filter(|s| !s.is_empty())
    }

    /// Set a value directly (for testing or one-shot updates).
    pub fn set(&self, name: &str, value: impl Into<String>) {
        if let Some(entry) = self.entries.get(name) {
            if let Ok(mut guard) = entry.output.lock() {
                *guard = value.into();
            }
        }
    }

    /// Returns the list of registered module names.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// True if there are no registered modules.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Drop for CustomModuleCache {
    fn drop(&mut self) {
        self.stop_all();
    }
}

// ── Convenience: build from config ────────────────────────────────────────────

use crate::config::{BarModuleKind, Config};

impl CustomModuleCache {
    /// Scan the config's `bar_modules` and register any `Custom` modules that
    /// have an `exec` or `shell` property (i.e. are command-backed).
    ///
    /// Config syntax (trixie.conf):
    ///
    /// ```
    /// bar_module cpu {
    ///     exec    = "top -bn1 | grep 'Cpu(s)' | awk '{print $2}'"
    ///     interval = 2
    /// }
    /// ```
    pub fn from_config(cfg: &Config) -> Self {
        let mut cache = Self::new();

        for (name, def) in &cfg.bar_modules {
            if def.kind != BarModuleKind::Custom {
                continue;
            }

            // `exec` property → shell command.
            if let Some(exec) = def.props.get("exec").and_then(|v| v.as_str()) {
                let interval: u64 = def
                    .props
                    .get("interval")
                    .and_then(|v| v.as_i64())
                    .map(|n| n.max(1) as u64)
                    .unwrap_or(5);

                tracing::info!("bar module '{name}': polling every {interval}s: {exec}");
                cache.register_shell(name.clone(), exec, interval);
            }
        }

        cache
    }
}
