//! Minimal Rust shell: spawns the Node core as a supervised child, hands the
//! WebView its endpoint (port + token), and stays out of everything else —
//! tray, window behavior, and all business logic live in TypeScript.

use rand::RngCore;
use std::sync::Mutex;
use tauri::{Manager, RunEvent};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;

struct CoreState {
    token: String,
    port: Mutex<Option<u16>>,
    child: Mutex<Option<CommandChild>>,
    exiting: Mutex<bool>,
}

#[tauri::command]
fn get_core_endpoint(state: tauri::State<'_, CoreState>) -> Result<serde_json::Value, String> {
    match *state.port.lock().unwrap() {
        Some(port) => Ok(serde_json::json!({ "port": port, "token": state.token })),
        None => Err("core-not-ready".into()),
    }
}

#[tauri::command]
fn is_hidden_launch() -> bool {
    std::env::args().any(|a| a == "--hidden")
}

fn make_token() -> String {
    let mut bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Builds the core command. Release: bundled node.exe sidecar + staged
/// resources. Dev (`tauri dev`): system `node` + the core package's build
/// output (run `pnpm --filter @yohaku/core build` first).
fn spawn_core(
    app: &tauri::AppHandle,
    token: &str,
) -> Result<(tauri::async_runtime::Receiver<CommandEvent>, CommandChild), String> {
    let shell = app.shell();
    let command = if cfg!(debug_assertions) {
        let core_dir = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join("../../core");
        shell
            .command("node")
            .args([core_dir.join("dist/main.cjs").to_string_lossy().to_string()])
            .env("YOHAKU_PS_DIR", core_dir.join("resources/ps").to_string_lossy().to_string())
    } else {
        // resource_dir() may carry a \\?\ verbatim prefix that Node's module
        // loader rejects; dunce-style trim keeps the path plain.
        let resources = app
            .path()
            .resource_dir()
            .map_err(|e| e.to_string())?
            .join("core");
        let plain = resources.to_string_lossy().replace("\\\\?\\", "");
        eprintln!("[shell] core resources at {plain}");
        shell
            .sidecar("yohaku-core-node")
            .map_err(|e| e.to_string())?
            .args([format!("{plain}\\main.cjs")])
            .env("YOHAKU_PS_DIR", format!("{plain}\\ps"))
    };
    command
        .env("YOHAKU_IPC_TOKEN", token)
        .spawn()
        .map_err(|e| e.to_string())
}

fn supervise_core(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut attempts: u32 = 0;
        loop {
            let token = app.state::<CoreState>().token.clone();
            let (mut rx, child) = match spawn_core(&app, &token) {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("[shell] failed to spawn core: {error}");
                    return;
                }
            };
            *app.state::<CoreState>().child.lock().unwrap() = Some(child);

            while let Some(event) = rx.recv().await {
                match event {
                    CommandEvent::Stdout(line) => {
                        if let Ok(value) =
                            serde_json::from_slice::<serde_json::Value>(&line)
                        {
                            if value["type"] == "ready" {
                                if let Some(port) = value["port"].as_u64() {
                                    *app.state::<CoreState>().port.lock().unwrap() =
                                        Some(port as u16);
                                    attempts = 0;
                                    let _ = app.emit_to(
                                        tauri::EventTarget::labeled("main"),
                                        "core-ready",
                                        port,
                                    );
                                }
                            }
                        }
                    }
                    CommandEvent::Stderr(line) => {
                        // Core logs are already content-redacted; surface them.
                        eprintln!("[core] {}", String::from_utf8_lossy(&line));
                    }
                    _ => {}
                }
            }

            // Channel closed: the core exited.
            *app.state::<CoreState>().port.lock().unwrap() = None;
            *app.state::<CoreState>().child.lock().unwrap() = None;
            if *app.state::<CoreState>().exiting.lock().unwrap() {
                return;
            }
            attempts += 1;
            if attempts > 5 {
                eprintln!("[shell] core restart limit reached");
                let _ = app.emit_to(
                    tauri::EventTarget::labeled("main"),
                    "core-dead",
                    (),
                );
                return;
            }
            let delay = std::time::Duration::from_millis(1000 * 2u64.pow(attempts - 1));
            eprintln!("[shell] core exited; restart {attempts}/5 in {delay:?}");
            tokio::time::sleep(delay).await;
        }
    });
}

use tauri::Emitter;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .manage(CoreState {
            token: make_token(),
            port: Mutex::new(None),
            child: Mutex::new(None),
            exiting: Mutex::new(false),
        })
        .invoke_handler(tauri::generate_handler![get_core_endpoint, is_hidden_launch])
        .setup(|app| {
            supervise_core(app.handle().clone());
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                *app.state::<CoreState>().exiting.lock().unwrap() = true;
                // The frontend already sent a `shutdown` command over the WS
                // (bounded remote clear); give the core a moment, then kill.
                if let Some(child) = app.state::<CoreState>().child.lock().unwrap().take()
                {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    let _ = child.kill();
                }
            }
        });
}
