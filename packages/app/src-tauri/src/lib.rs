//! Tauri shell around the native `yohaku-core` crate: builds the core
//! service graph in `setup` (state.rs), exposes the IPC command surface
//! (commands.rs), and keeps the tray/window/updater behavior. The old Node
//! sidecar + WebSocket IPC are gone — the UI talks to the core via
//! `invoke` + the `core-state` event.

mod commands;
mod state;

use serde::Serialize;
use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_updater::UpdaterExt;
use url::Url;

const GITHUB_UPDATE_ENDPOINT: &str =
    "https://github.com/whitebearovo/YohakuCompanion-Win/releases/latest/download/latest.json";
const MIRROR_PREFIX: &str = "https://ghfast.top/";

#[derive(Clone, Serialize)]
struct UpdaterProgress {
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
    version: Option<String>,
}

fn emit_updater_progress(
    app: &tauri::AppHandle,
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
    version: Option<String>,
) {
    let _ = app.emit(
        "updater-progress",
        UpdaterProgress {
            phase,
            downloaded,
            total,
            version,
        },
    );
}

#[tauri::command]
async fn check_and_install_update(app: tauri::AppHandle, source: String) -> Result<(), String> {
    let use_mirror = match source.as_str() {
        "github" => false,
        "mirror" => true,
        _ => return Err("invalid update source".into()),
    };
    let endpoint = if use_mirror {
        format!("{MIRROR_PREFIX}{GITHUB_UPDATE_ENDPOINT}")
    } else {
        GITHUB_UPDATE_ENDPOINT.to_string()
    };
    let endpoint = Url::parse(&endpoint).map_err(|error| error.to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;

    let Some(mut update) = updater.check().await.map_err(|error| error.to_string())? else {
        emit_updater_progress(&app, "upToDate", 0, None, None);
        return Ok(());
    };

    let version = update.version.clone();
    // The manifest contains the canonical GitHub asset URL. Mirror mode only
    // prefixes that URL, leaving the signed bytes and signature unchanged.
    if use_mirror && !update.download_url.as_str().starts_with(MIRROR_PREFIX) {
        let mirrored_url = format!("{MIRROR_PREFIX}{}", update.download_url);
        update.download_url = Url::parse(&mirrored_url).map_err(|error| error.to_string())?;
    }

    emit_updater_progress(&app, "downloading", 0, None, Some(version.clone()));
    let progress_app = app.clone();
    let finish_app = app.clone();
    let chunk_version = version.clone();
    let mut downloaded = 0u64;
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                emit_updater_progress(
                    &progress_app,
                    "downloading",
                    downloaded,
                    total,
                    Some(chunk_version.clone()),
                );
            },
            move || {
                emit_updater_progress(&finish_app, "verifying", 0, None, Some(version));
            },
        )
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn is_hidden_launch() -> bool {
    std::env::args().any(|a| a == "--hidden")
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::pair,
            commands::unpair,
            commands::request_preview,
            commands::confirm_consent,
            commands::disable_live_desk,
            commands::set_sources,
            commands::set_privacy,
            commands::upsert_rule,
            commands::delete_rule,
            commands::set_mappings,
            commands::shutdown,
            is_hidden_launch,
            check_and_install_update
        ])
        .setup(|app| {
            state::init(app.handle()).map_err(|error| -> Box<dyn std::error::Error> {
                format!("core init failed: {error}").into()
            })?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Bounded remote clear + teardown (2 s), port of gracefulExit.
                if let Some(core) = app.try_state::<state::AppCoreState>() {
                    core.shutdown_blocking();
                }
            }
        });
}
