//! Tauri commands mirroring the old WebSocket IPC `Command` union
//! (`packages/shared/src/ipc.ts`) routed per `packages/core/src/ipc/handlers.ts`:
//! privacy mutations go through `ConfigStore.update` (atomic write) followed
//! by `policy_maybe_changed()` so the consent gate and Live Desk always
//! observe the new fingerprint; connection commands delegate to the service.
//!
//! Every command returns `Result<_, String>` where the `Err` string is the
//! `IpcErrorCode` literal (the UI maps it to a label).

use std::cmp::Ordering;

use serde::Deserialize;
use tauri::State;
use yohaku_core::companion::service::ServiceError;
use yohaku_core::model::{
    ApplicationPrivacyRule, CoreStateSnapshot, IpcErrorCode, PrivacyDefault, PrivacyMapping,
};
use yohaku_core::privacy::model::{is_empty_rule, normalized_rule};

use crate::state::AppCoreState;

/// `privacyPatchSchema` — every key optional; absent means "keep current".
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyPatchDefaults {
    pub application: Option<PrivacyDefault>,
    pub window_title: Option<PrivacyDefault>,
    pub media: Option<PrivacyDefault>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyPatch {
    pub defaults: Option<PrivacyPatchDefaults>,
    pub share_window_titles: Option<bool>,
    pub ignore_null_artist: Option<bool>,
}

fn service_error(error: ServiceError) -> String {
    error.code.as_str().to_string()
}

/// Non-ServiceError failures (config write, etc.) map to `"internal"`,
/// mirroring the TS command handler's catch-all.
fn internal() -> String {
    IpcErrorCode::Internal.as_str().to_string()
}

/// zod `z.string().min(1)` on command fields.
fn require_non_empty(values: &[&str]) -> Result<(), String> {
    if values.iter().any(|value| value.is_empty()) {
        return Err(IpcErrorCode::InvalidInput.as_str().to_string());
    }
    Ok(())
}

/// JS `<` on strings compares UTF-16 code units (rule sort in handlers.ts).
fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[tauri::command]
pub fn get_state(state: State<'_, AppCoreState>) -> CoreStateSnapshot {
    state.snapshot()
}

#[tauri::command]
pub async fn pair(
    state: State<'_, AppCoreState>,
    base_url: String,
    device_name: String,
    pairing_code: String,
) -> Result<(), String> {
    require_non_empty(&[&base_url, &device_name, &pairing_code])?;
    state
        .service
        .pair(&base_url, &device_name, &pairing_code)
        .await
        .map_err(service_error)
}

#[tauri::command]
pub async fn unpair(state: State<'_, AppCoreState>) -> Result<(), String> {
    state.service.unpair().await.map_err(service_error)
}

#[tauri::command]
pub async fn request_preview(state: State<'_, AppCoreState>) -> Result<(), String> {
    // The fresh preview reaches the UI through the snapshot broadcast
    // (`on_changed`), exactly like the TS core (the standalone "preview"
    // server message was never emitted).
    state
        .service
        .refresh_preview()
        .await
        .map(|_| ())
        .map_err(service_error)
}

#[tauri::command]
pub async fn confirm_consent(
    state: State<'_, AppCoreState>,
    policy_fingerprint: String,
) -> Result<(), String> {
    require_non_empty(&[&policy_fingerprint])?;
    state
        .service
        .confirm_consent(&policy_fingerprint)
        .await
        .map_err(service_error)
}

#[tauri::command]
pub async fn disable_live_desk(state: State<'_, AppCoreState>) -> Result<(), String> {
    state
        .service
        .disable_live_desk()
        .await
        .map_err(service_error)
}

#[tauri::command]
pub fn set_sources(
    state: State<'_, AppCoreState>,
    application: Option<bool>,
    media: Option<bool>,
) -> Result<(), String> {
    state
        .config
        .update(|c| {
            c.privacy.sources.application = application.unwrap_or(c.privacy.sources.application);
            c.privacy.sources.media = media.unwrap_or(c.privacy.sources.media);
        })
        .map_err(|_| internal())?;
    state.service.policy_maybe_changed();
    Ok(())
}

#[tauri::command]
pub fn set_privacy(state: State<'_, AppCoreState>, patch: PrivacyPatch) -> Result<(), String> {
    state
        .config
        .update(|c| {
            if let Some(defaults) = &patch.defaults {
                c.privacy.defaults.application = defaults
                    .application
                    .unwrap_or(c.privacy.defaults.application);
                c.privacy.defaults.window_title = defaults
                    .window_title
                    .unwrap_or(c.privacy.defaults.window_title);
                c.privacy.defaults.media = defaults.media.unwrap_or(c.privacy.defaults.media);
            }
            c.privacy.share_window_titles = patch
                .share_window_titles
                .unwrap_or(c.privacy.share_window_titles);
            c.privacy.ignore_null_artist = patch
                .ignore_null_artist
                .unwrap_or(c.privacy.ignore_null_artist);
        })
        .map_err(|_| internal())?;
    state.service.policy_maybe_changed();
    Ok(())
}

#[tauri::command]
pub fn upsert_rule(
    state: State<'_, AppCoreState>,
    rule: ApplicationPrivacyRule,
) -> Result<(), String> {
    require_non_empty(&[&rule.app_id])?;
    let rule = normalized_rule(&rule);
    state
        .config
        .update(|c| {
            c.privacy.rules.retain(|r| r.app_id != rule.app_id);
            if !is_empty_rule(&rule) {
                c.privacy.rules.push(rule);
            }
            c.privacy
                .rules
                .sort_by(|a, b| cmp_utf16(&a.app_id, &b.app_id));
        })
        .map_err(|_| internal())?;
    state.service.policy_maybe_changed();
    Ok(())
}

#[tauri::command]
pub fn delete_rule(state: State<'_, AppCoreState>, app_id: String) -> Result<(), String> {
    require_non_empty(&[&app_id])?;
    let needle = app_id.to_lowercase();
    state
        .config
        .update(|c| c.privacy.rules.retain(|r| r.app_id != needle))
        .map_err(|_| internal())?;
    state.service.policy_maybe_changed();
    Ok(())
}

#[tauri::command]
pub fn set_mappings(
    state: State<'_, AppCoreState>,
    mappings: Vec<PrivacyMapping>,
) -> Result<(), String> {
    state
        .config
        .update(|c| c.privacy.mappings = mappings)
        .map_err(|_| internal())?;
    state.service.policy_maybe_changed();
    Ok(())
}

#[tauri::command]
pub fn shutdown(app: tauri::AppHandle) {
    // RunEvent::Exit runs the bounded core shutdown (state.rs).
    app.exit(0);
}
