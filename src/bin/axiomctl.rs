use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: axiomctl <command> [args...]");
        eprintln!("Commands: clients, workspaces, active-window, version,");
        eprintln!("          switch-workspace <n>, close [id], toggle-float,");
        eprintln!("          toggle-fullscreen, set-layout <name>,");
        eprintln!("          exec <cmd>, reload, exit, lua <code>");
        std::process::exit(1);
    }

    let request = build_request(&args);
    let socket = socket_path();

    let mut stream = UnixStream::connect(&socket).unwrap_or_else(|e| {
        eprintln!("Cannot connect to {socket}: {e}");
        std::process::exit(1);
    });

    stream.write_all(request.as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    println!("{}", resp.trim());
}

fn build_request(args: &[String]) -> String {
    let obj = match args[0].as_str() {
        "clients" => serde_json::json!({ "cmd": "clients" }),
        "workspaces" => serde_json::json!({ "cmd": "workspaces" }),
        "monitors" => serde_json::json!({ "cmd": "monitors" }),
        "active-window" => serde_json::json!({ "cmd": "active_window" }),
        "version" => serde_json::json!({ "cmd": "version" }),
        "toggle-float" => serde_json::json!({ "cmd": "toggle_float" }),
        "toggle-fullscreen" => serde_json::json!({ "cmd": "toggle_fullscreen" }),
        "reload" => serde_json::json!({ "cmd": "reload" }),
        "exit" => serde_json::json!({ "cmd": "exit" }),
        "switch-workspace" => {
            let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
            serde_json::json!({ "cmd": "switch_workspace", "workspace": n })
        }
        "close" => {
            let id: Option<u32> = args.get(1).and_then(|s| s.parse().ok());
            serde_json::json!({ "cmd": "close_window", "id": id })
        }
        "set-layout" => {
            let layout = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "master_stack".to_string());
            serde_json::json!({ "cmd": "set_layout", "layout": layout })
        }
        "exec" => {
            let cmd = args[1..].join(" ");
            serde_json::json!({ "cmd": "exec", "command": cmd })
        }
        "lua" => {
            let code = args[1..].join(" ");
            serde_json::json!({ "cmd": "lua", "code": code })
        }
        other => {
            eprintln!("Unknown command: {other}");
            std::process::exit(1);
        }
    };
    obj.to_string()
}

fn socket_path() -> String {
    if let Ok(p) = std::env::var("AXIOM_SOCKET") {
        return p;
    }
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{runtime}/axiom-{display}.sock")
}
