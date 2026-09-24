//! config.json — non-secret configuration only. The device token NEVER
//! lives here.
//!
//! Owned by agent S. Ground truth:
//! `packages/core/src/store/configStore.ts` and `capture-stores.md` §7.
//! Atomic writes (tmp + fsync + rename); a corrupt file is preserved as
//! `.bak` (best-effort, overwriting) and replaced with defaults in memory
//! only (fail-closed: losing `connection` means "not paired", never
//! "publishing enabled"); a MISSING file silently yields defaults with no
//! log/backup/write — the file first appears on the next `update()`.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde::Serialize;

use crate::model::{default_core_config, CoreConfig, PrivacyConfig, Unsubscribe};
use crate::runtime::logger;

#[derive(Debug, thiserror::Error)]
pub enum ConfigStoreError {
    /// `YOHAKU_DATA_DIR` unset AND `%APPDATA%` unset/empty
    /// (TS: `"APPDATA is not set"`), or directory creation failed.
    #[error("data directory unavailable: {0}")]
    DataDirUnavailable(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// `update` produced a config that fails schema validation — nothing is
    /// written and the in-memory config is unchanged.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// `YOHAKU_DATA_DIR` env override (set AND non-empty) verbatim, else
/// `%APPDATA%\yohaku-companion-win`.
pub fn data_directory() -> Result<PathBuf, ConfigStoreError> {
    resolve_data_directory(
        std::env::var_os("YOHAKU_DATA_DIR"),
        std::env::var_os("APPDATA"),
    )
}

/// Pure resolution core of [`data_directory`], split out so tests need not
/// mutate process-global environment variables.
fn resolve_data_directory(
    override_dir: Option<OsString>,
    app_data: Option<OsString>,
) -> Result<PathBuf, ConfigStoreError> {
    if let Some(dir) = override_dir {
        // TS: used verbatim only when set AND non-empty.
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    match app_data {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("yohaku-companion-win")),
        _ => Err(ConfigStoreError::DataDirUnavailable(
            "APPDATA is not set".to_string(),
        )),
    }
}

/// Byte-for-byte TS `atomicWriteJson`: write `JSON.stringify(value, null,
/// 2)` equivalent (UTF-8, no BOM, 2-space indent, `\n`, NO trailing
/// newline) to `<path>.tmp`, fsync BEFORE close, then rename over `path`
/// (replace semantics — `std::fs::rename` replaces existing files on
/// Windows). No directory fsync (accepted risk). Used by config.json,
/// sequence.json and credentials.bin.json.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    // serde_json pretty formatting matches JSON.stringify(v, null, 2):
    // 2-space indent, `": "` key separator, `\n` line breaks, no trailing
    // newline, non-ASCII left unescaped.
    let json = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let tmp = sibling_with_suffix(path, ".tmp");
    let mut file = fs::File::create(&tmp)?;
    // TS shape: write + fsync in `try`, close in `finally`, rename after.
    // A write/fsync failure still closes the fd (drop) and skips the rename.
    let write_result = file
        .write_all(json.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    write_result?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// `<path><suffix>` (e.g. `config.json.tmp`, `config.json.bak`) without
/// lossy round-trips through `str` — Windows paths are not always UTF-8.
fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_os_string();
    os.push(suffix);
    PathBuf::from(os)
}

/// Lenient object-file reader shared by the sequence and DPAPI stores
/// (mirrors their duplicated private TS `read()` helpers): the parsed value
/// must be a JSON object — anything else (missing file, unreadable bytes,
/// parse error, `null`, array, scalar) reads as an empty map. No `.bak`, no
/// logging.
pub(crate) fn read_json_object(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    let Ok(bytes) = fs::read(path) else {
        return serde_json::Map::new();
    };
    // Node read with "utf8" decodes lossily; match so a mojibake file takes
    // the parse-error path rather than a read-error path.
    let text = String::from_utf8_lossy(&bytes);
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

/// Zod refinements not expressible in the `model.rs` serde derives; applied
/// wherever TS ran `coreConfigSchema.parse` (load + update). Everything else
/// (version literal, enums, types, required keys, unknown-key stripping) is
/// enforced by serde itself.
fn validate_config_semantics(config: &CoreConfig) -> Result<(), String> {
    for rule in &config.privacy.rules {
        if rule.app_id.is_empty() {
            return Err("privacy.rules[].appId must be non-empty".to_string());
        }
    }
    for mapping in &config.privacy.mappings {
        if mapping.from.is_empty() {
            return Err("privacy.mappings[].from must be non-empty".to_string());
        }
        if mapping.to.is_empty() {
            return Err("privacy.mappings[].to must be non-empty".to_string());
        }
    }
    // connection.pairingNextSequence >= 0 and integer are type-enforced (u64).
    Ok(())
}

/// Full-schema parse of on-disk text (serde + semantic refinements) — the
/// equivalent of `coreConfigSchema.parse(JSON.parse(raw))`.
fn parse_config(text: &str) -> Result<CoreConfig, String> {
    let config: CoreConfig = serde_json::from_str(text).map_err(|e| e.to_string())?;
    validate_config_semantics(&config)?;
    Ok(config)
}

/// Re-validation of an in-memory mutation result: serde round-trip (runs the
/// literal/shape deserializers, e.g. `version == 1`) + semantic refinements.
/// Equivalent to the TS `coreConfigSchema.parse(mutate(clone))`.
fn revalidate(config: &CoreConfig) -> Result<CoreConfig, String> {
    let value = serde_json::to_value(config).map_err(|e| e.to_string())?;
    let parsed: CoreConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
    validate_config_semantics(&parsed)?;
    Ok(parsed)
}

type ListenerFn = Arc<dyn Fn(&CoreConfig) + Send + Sync>;
type Listeners = Mutex<Vec<(u64, ListenerFn)>>;

/// In-memory config with synchronous change listeners.
pub struct ConfigStore {
    path: PathBuf,
    config: Mutex<CoreConfig>,
    listeners: Arc<Listeners>,
    next_listener_id: AtomicU64,
    /// Serializes whole `update` calls (clone -> mutate -> write -> swap ->
    /// notify) so concurrent updates cannot interleave their read-modify-
    /// write cycles. All work under it is synchronous (never held across an
    /// `.await`).
    update_lock: Mutex<()>,
}

impl ConfigStore {
    /// `directory = None` uses [`data_directory`]; the directory is created
    /// recursively; then the file is loaded per the corrupt-handling rules.
    pub fn new(directory: Option<PathBuf>) -> Result<Self, ConfigStoreError> {
        let directory = match directory {
            Some(directory) => directory,
            None => data_directory()?,
        };
        fs::create_dir_all(&directory)
            .map_err(|e| ConfigStoreError::DataDirUnavailable(e.to_string()))?;
        let path = directory.join("config.json");
        let config = Self::load(&path);
        Ok(Self {
            path,
            config: Mutex::new(config),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: AtomicU64::new(0),
            update_lock: Mutex::new(()),
        })
    }

    /// Load-or-defaults per configStore.ts:
    /// - read error (missing file, access) -> defaults SILENTLY (no log, no
    ///   backup, nothing written to disk);
    /// - parse/validation error -> log, best-effort copy to `.bak`
    ///   (overwriting an existing one), defaults in memory only — the
    ///   corrupt file stays in place until the next `update()`.
    fn load(path: &Path) -> CoreConfig {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(_) => return default_core_config(),
        };
        let text = String::from_utf8_lossy(&bytes);
        match parse_config(&text) {
            Ok(config) => config,
            Err(_) => {
                logger::error(
                    "config",
                    "config.json corrupt; backing up and using defaults",
                );
                // Backup is best-effort; failures are swallowed.
                let _ = fs::copy(path, sibling_with_suffix(path, ".bak"));
                default_core_config()
            }
        }
    }

    /// Current config (a CLONE — the TS returned its live internal object;
    /// the Rust contract clones, capture-stores spec ambiguity §13.6).
    pub fn get(&self) -> CoreConfig {
        self.config.lock().clone()
    }

    /// Convenience: `get().privacy`.
    pub fn privacy(&self) -> PrivacyConfig {
        self.config.lock().privacy.clone()
    }

    /// Change listeners are notified SYNCHRONOUSLY after a successful
    /// `update`, with the new config.
    pub fn on_change(&self, callback: Box<dyn Fn(&CoreConfig) + Send + Sync>) -> Unsubscribe {
        let id = self.next_listener_id.fetch_add(1, Ordering::Relaxed);
        self.listeners.lock().push((id, Arc::from(callback)));
        let weak: Weak<Listeners> = Arc::downgrade(&self.listeners);
        Unsubscribe::new(move || {
            if let Some(listeners) = weak.upgrade() {
                listeners
                    .lock()
                    .retain(|(listener_id, _)| *listener_id != id);
            }
        })
    }

    /// Clone current -> `mutate` -> re-validate the WHOLE schema (invalid ->
    /// `Err`, nothing written) -> atomic write -> swap in memory -> notify
    /// listeners -> return the new config.
    pub fn update(
        &self,
        mutate: impl FnOnce(&mut CoreConfig),
    ) -> Result<CoreConfig, ConfigStoreError> {
        let _serialized = self.update_lock.lock();
        let mut next = self.config.lock().clone();
        mutate(&mut next);
        let next = revalidate(&next).map_err(ConfigStoreError::Invalid)?;
        atomic_write_json(&self.path, &next)?;
        *self.config.lock() = next.clone();
        // Snapshot the callbacks, then invoke without holding the listeners
        // lock so a listener may re-enter on_change/update/get.
        let callbacks: Vec<ListenerFn> = self
            .listeners
            .lock()
            .iter()
            .map(|(_, callback)| callback.clone())
            .collect();
        for callback in callbacks {
            callback(&next);
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CredentialBackend, StoredConnection};

    /// Unique per-test directory under the OS temp dir; removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("yohaku-core-config-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }

        fn file(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    // -- data_directory -----------------------------------------------------

    #[test]
    fn data_dir_override_used_verbatim() {
        let dir =
            resolve_data_directory(os("D:\\custom\\data"), os("C:\\Users\\u\\AppData\\Roaming"))
                .unwrap();
        assert_eq!(dir, PathBuf::from("D:\\custom\\data"));
    }

    #[test]
    fn data_dir_empty_override_falls_back_to_appdata() {
        let dir = resolve_data_directory(os(""), os("C:\\Users\\u\\AppData\\Roaming")).unwrap();
        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\u\\AppData\\Roaming").join("yohaku-companion-win")
        );
    }

    #[test]
    fn data_dir_appdata_joined_with_app_folder() {
        let dir = resolve_data_directory(None, os("C:\\Users\\u\\AppData\\Roaming")).unwrap();
        assert_eq!(
            dir,
            PathBuf::from("C:\\Users\\u\\AppData\\Roaming\\yohaku-companion-win")
        );
    }

    #[test]
    fn data_dir_missing_or_empty_appdata_errors() {
        for app_data in [None, os("")] {
            let error = resolve_data_directory(None, app_data).unwrap_err();
            match error {
                ConfigStoreError::DataDirUnavailable(message) => {
                    assert_eq!(message, "APPDATA is not set"); // exact TS message
                }
                other => panic!("expected DataDirUnavailable, got {other:?}"),
            }
        }
    }

    // -- atomic_write_json --------------------------------------------------

    #[test]
    fn atomic_write_json_exact_bytes() {
        let tmp = TempDir::new();
        let path = tmp.file("out.json");
        let mut map = serde_json::Map::new();
        map.insert("a".to_string(), serde_json::Value::from(1));
        atomic_write_json(&path, &map).unwrap();
        let bytes = fs::read(&path).unwrap();
        // JSON.stringify({a: 1}, null, 2): 2-space indent, \n, no trailing
        // newline, no BOM, no \r.
        assert_eq!(bytes, b"{\n  \"a\": 1\n}");
        assert!(
            !tmp.file("out.json.tmp").exists(),
            "tmp file must be renamed away"
        );
    }

    #[test]
    fn atomic_write_json_empty_object_and_replace() {
        let tmp = TempDir::new();
        let path = tmp.file("out.json");
        atomic_write_json(&path, &serde_json::Map::new()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{}");
        // Rename must REPLACE an existing destination (Windows semantics).
        let mut map = serde_json::Map::new();
        map.insert("k".to_string(), serde_json::Value::from(2));
        atomic_write_json(&path, &map).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\n  \"k\": 2\n}");
    }

    // -- defaults / load ----------------------------------------------------

    #[test]
    fn missing_file_yields_defaults_silently() {
        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        assert_eq!(store.get(), default_core_config());
        // The file first appears on the next update(); no .bak either.
        assert!(!tmp.file("config.json").exists());
        assert!(!tmp.file("config.json.bak").exists());
    }

    #[test]
    fn fresh_default_matches_ts_stringify_bytes() {
        // Byte-compat contract: what a fresh install writes on the first
        // update() must equal TS JSON.stringify(defaultCoreConfig(), null, 2).
        let expected = "{\n  \"version\": 1,\n  \"privacy\": {\n    \"defaults\": {\n      \"application\": \"share\",\n      \"windowTitle\": \"hide\",\n      \"media\": \"share\"\n    },\n    \"rules\": [],\n    \"mappings\": [],\n    \"shareWindowTitles\": false,\n    \"ignoreNullArtist\": false,\n    \"sources\": {\n      \"application\": true,\n      \"media\": true\n    }\n  },\n  \"connection\": null,\n  \"media\": {\n    \"provider\": \"auto\"\n  },\n  \"credentialBackend\": null\n}";
        assert_eq!(
            serde_json::to_string_pretty(&default_core_config()).unwrap(),
            expected
        );

        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        store.update(|_| {}).unwrap(); // no-op update materializes the file
        assert_eq!(
            fs::read(tmp.file("config.json")).unwrap(),
            expected.as_bytes()
        );
    }

    #[test]
    fn corrupt_file_backed_up_and_defaults_used() {
        let tmp = TempDir::new();
        let garbage = b"not json {{{";
        fs::write(tmp.file("config.json"), garbage).unwrap();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        assert_eq!(store.get(), default_core_config());
        // .bak holds the corrupt bytes; the corrupt original is left in
        // place until the next update() overwrites it.
        assert_eq!(fs::read(tmp.file("config.json.bak")).unwrap(), garbage);
        assert_eq!(fs::read(tmp.file("config.json")).unwrap(), garbage);
        store
            .update(|c| c.privacy.share_window_titles = true)
            .unwrap();
        let reloaded = ConfigStore::new(Some(tmp.path())).unwrap();
        assert!(reloaded.get().privacy.share_window_titles);
    }

    #[test]
    fn wrong_version_is_corrupt() {
        let tmp = TempDir::new();
        let mut value = serde_json::to_value(default_core_config()).unwrap();
        value["version"] = serde_json::Value::from(2);
        fs::write(
            tmp.file("config.json"),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        assert_eq!(store.get(), default_core_config());
        assert!(tmp.file("config.json.bak").exists());
    }

    #[test]
    fn invalid_enum_or_negative_sequence_is_corrupt() {
        let tmp = TempDir::new();
        for mutate in [
            |v: &mut serde_json::Value| v["media"]["provider"] = "vlc".into(),
            |v: &mut serde_json::Value| {
                v["connection"] = serde_json::json!({
                    "baseUrl": "https://x", "deviceId": "d", "deviceName": "n",
                    "scopes": [], "pairingNextSequence": -1, "liveDeskEnabled": false
                });
            },
            |v: &mut serde_json::Value| v["privacy"]["rules"] = serde_json::json!([{ "appId": "", "application": "inherit", "windowTitle": "inherit", "media": "inherit" }]),
        ] {
            let mut value = serde_json::to_value(default_core_config()).unwrap();
            mutate(&mut value);
            fs::write(
                tmp.file("config.json"),
                serde_json::to_string(&value).unwrap(),
            )
            .unwrap();
            let store = ConfigStore::new(Some(tmp.path())).unwrap();
            assert_eq!(
                store.get(),
                default_core_config(),
                "whole parse must fail (no partial recovery)"
            );
        }
    }

    #[test]
    fn unknown_keys_are_ignored_on_read() {
        let tmp = TempDir::new();
        let mut value = serde_json::to_value(default_core_config()).unwrap();
        value["futureKey"] = serde_json::json!({ "nested": true });
        value["privacy"]["extra"] = serde_json::Value::from(1);
        fs::write(
            tmp.file("config.json"),
            serde_json::to_string(&value).unwrap(),
        )
        .unwrap();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        // zod strip semantics: parse succeeds, known values preserved.
        assert_eq!(store.get(), default_core_config());
        assert!(!tmp.file("config.json.bak").exists());
    }

    // -- update -------------------------------------------------------------

    fn sample_connection() -> StoredConnection {
        StoredConnection {
            base_url: "https://yohaku.example".to_string(),
            device_id: "01J8ME9FZW3W7T2C4Y8K5Q6R9S".to_string(),
            device_name: "Desk".to_string(),
            scopes: vec!["presence:write".to_string()],
            pairing_next_sequence: 7,
            live_desk_enabled: true,
        }
    }

    #[test]
    fn update_persists_and_round_trips() {
        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        let updated = store
            .update(|c| {
                c.connection = Some(sample_connection());
                c.credential_backend = Some(CredentialBackend::Keyring);
                c.privacy.share_window_titles = true;
            })
            .unwrap();
        assert_eq!(updated.connection.as_ref(), Some(&sample_connection()));
        assert_eq!(store.get(), updated);

        let reloaded = ConfigStore::new(Some(tmp.path())).unwrap();
        assert_eq!(reloaded.get(), updated);

        // On-disk shape spot checks (camelCase keys, exact file layout).
        let text = fs::read_to_string(tmp.file("config.json")).unwrap();
        assert!(text.contains("\"pairingNextSequence\": 7"));
        assert!(text.contains("\"credentialBackend\": \"keyring\""));
        assert!(!text.ends_with('\n'));
        assert!(!text.contains('\r'));
        assert!(!tmp.file("config.json.tmp").exists());
    }

    #[test]
    fn display_alias_key_absent_unless_set() {
        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        store
            .update(|c| {
                c.privacy.rules = vec![
                    crate::model::ApplicationPrivacyRule {
                        app_id: "code.exe".to_string(),
                        application: crate::model::PrivacyOverride::Inherit,
                        window_title: crate::model::PrivacyOverride::Hide,
                        media: crate::model::PrivacyOverride::Inherit,
                        display_alias: None,
                    },
                    crate::model::ApplicationPrivacyRule {
                        app_id: "spotify.exe".to_string(),
                        application: crate::model::PrivacyOverride::Share,
                        window_title: crate::model::PrivacyOverride::Inherit,
                        media: crate::model::PrivacyOverride::Inherit,
                        display_alias: Some("Music".to_string()),
                    },
                ];
            })
            .unwrap();
        let text = fs::read_to_string(tmp.file("config.json")).unwrap();
        // Optional key: present only when set, never `"displayAlias": null`.
        assert_eq!(text.matches("displayAlias").count(), 1);
        assert!(text.contains("\"displayAlias\": \"Music\""));
    }

    #[test]
    fn invalid_update_writes_nothing_and_keeps_memory() {
        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        let notified = Arc::new(Mutex::new(0_usize));
        let seen = notified.clone();
        let guard = store.on_change(Box::new(move |_| *seen.lock() += 1));

        let error = store.update(|c| c.version = 2).unwrap_err();
        assert!(matches!(error, ConfigStoreError::Invalid(_)));
        assert_eq!(
            store.get(),
            default_core_config(),
            "in-memory config unchanged"
        );
        assert!(
            !tmp.file("config.json").exists(),
            "nothing written on invalid update"
        );
        assert_eq!(*notified.lock(), 0, "listeners not called on failed update");

        let error = store.update(|c| {
            c.privacy.mappings = vec![crate::model::PrivacyMapping {
                mapping_type: crate::model::PrivacyMappingType::ProcessName,
                from: String::new(),
                to: "x".to_string(),
            }];
        });
        assert!(matches!(error, Err(ConfigStoreError::Invalid(_))));
        drop(guard);
    }

    #[test]
    fn on_change_notifies_and_unsubscribes() {
        let tmp = TempDir::new();
        let store = ConfigStore::new(Some(tmp.path())).unwrap();
        let received: Arc<Mutex<Vec<CoreConfig>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = received.clone();
        let guard = store.on_change(Box::new(move |config| sink.lock().push(config.clone())));

        let first = store
            .update(|c| c.privacy.ignore_null_artist = true)
            .unwrap();
        assert_eq!(received.lock().as_slice(), &[first]);

        guard.unsubscribe();
        store
            .update(|c| c.privacy.ignore_null_artist = false)
            .unwrap();
        assert_eq!(
            received.lock().len(),
            1,
            "unsubscribed listener must not fire"
        );

        // detach() keeps the callback for the store's lifetime.
        let sink = received.clone();
        store
            .on_change(Box::new(move |config| sink.lock().push(config.clone())))
            .detach();
        store
            .update(|c| c.privacy.share_window_titles = true)
            .unwrap();
        assert_eq!(received.lock().len(), 2);
    }
}
