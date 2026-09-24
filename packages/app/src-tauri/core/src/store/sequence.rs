//! sequence.json — per-device next-sequence persistence.
//!
//! Owned by agent S. Ground truth:
//! `packages/core/src/store/sequenceStore.ts` and `capture-stores.md` §8.
//! Deliberately a SEPARATE file from config.json: correctness writes
//! (reserve-before-send) must not race the chattier settings writes.
//! Layout: one JSON object mapping deviceId -> number, written with the
//! `atomic_write_json` protocol (2-space indent). Missing/corrupt file
//! reads as `{}` — no `.bak`, no logging; the sequencer self-heals via its
//! pairing floor. `remove` keeps an empty `{}` file (unlike the DPAPI
//! store, which deletes its file when it empties).

use std::path::PathBuf;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::protocol::sequencer::SequenceBacking;
use crate::store::config::{atomic_write_json, data_directory, read_json_object};

pub struct FileSequenceStore {
    path: PathBuf,
    /// Serializes read-modify-write cycles within this store. The sequencer
    /// already funnels reserve/reconcile through its own FIFO section; this
    /// guard additionally keeps `remove` (unpair) from tearing a concurrent
    /// `store`. Held only across synchronous file IO — never an `.await`.
    io_lock: Mutex<()>,
}

impl FileSequenceStore {
    /// `directory = None` uses `store::config::data_directory()`; the
    /// directory is created recursively.
    pub fn new(directory: Option<PathBuf>) -> Result<Self, std::io::Error> {
        let directory = match directory {
            Some(directory) => directory,
            None => data_directory().map_err(std::io::Error::other)?,
        };
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            path: directory.join("sequence.json"),
            io_lock: Mutex::new(()),
        })
    }

    /// Per-operation re-read (no caching): non-object/missing/corrupt -> `{}`
    /// ("sequencer self-heals via reconcile").
    fn read(&self) -> serde_json::Map<String, serde_json::Value> {
        read_json_object(&self.path)
    }

    /// Raw stored value for `device_id`: the JSON number if present (a
    /// non-integer JSON number maps to `None` — behavior-equivalent merge
    /// of the TS "any number, sequencer rejects non-integers" layering),
    /// else `None`. Negative INTEGERS pass through as `Some(-n)`; the
    /// sequencer judges validity (0..=2^53-1) and self-heals to the pairing
    /// floor. No caching: every call re-reads the file.
    pub async fn load(&self, device_id: &str) -> Option<i64> {
        let _guard = self.io_lock.lock();
        // serde_json `as_i64`: Some only for integer-representable values —
        // floats (41.5, 1e300) and out-of-i64-range integers map to None,
        // which the sequencer treats exactly like the TS invalid-number path.
        self.read()
            .get(device_id)
            .and_then(serde_json::Value::as_i64)
    }

    /// Read-modify-write with the atomic protocol. Failure must propagate
    /// (the sequencer treats it as "nothing consumed").
    pub async fn store(&self, device_id: &str, next: u64) -> std::io::Result<()> {
        let _guard = self.io_lock.lock();
        let mut all = self.read();
        all.insert(device_id.to_string(), serde_json::Value::from(next));
        atomic_write_json(&self.path, &all)
    }

    /// Only touches the disk when the key exists; deletes the key and
    /// atomically writes the remainder (an empty `{}` stays on disk). Used
    /// on unpair.
    pub async fn remove(&self, device_id: &str) -> std::io::Result<()> {
        let _guard = self.io_lock.lock();
        let mut all = self.read();
        if all.remove(device_id).is_some() {
            atomic_write_json(&self.path, &all)?;
        }
        Ok(())
    }
}

#[async_trait]
impl SequenceBacking for FileSequenceStore {
    async fn load(&self, device_id: &str) -> Option<i64> {
        FileSequenceStore::load(self, device_id).await
    }

    async fn store(&self, device_id: &str, next: u64) -> std::io::Result<()> {
        FileSequenceStore::store(self, device_id, next).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("yohaku-core-seq-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }

        fn file(&self) -> PathBuf {
            self.0.join("sequence.json")
        }

        fn write(&self, contents: &str) {
            fs::write(self.file(), contents).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store_in(dir: &TempDir) -> FileSequenceStore {
        FileSequenceStore::new(Some(dir.path())).unwrap()
    }

    const DEVICE: &str = "01J8ME9FZW3W7T2C4Y8K5Q6R9S";

    #[tokio::test]
    async fn store_then_load_round_trips_with_exact_layout() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.store(DEVICE, 42).await.unwrap();
        assert_eq!(store.load(DEVICE).await, Some(42));
        // Spec §8 example layout, byte-for-byte (atomic protocol, 2-space
        // indent, no trailing newline).
        assert_eq!(
            fs::read(tmp.file()).unwrap(),
            format!("{{\n  \"{DEVICE}\": 42\n}}").as_bytes()
        );
        assert!(!Path::new(&format!("{}.tmp", tmp.file().display())).exists());
    }

    #[tokio::test]
    async fn missing_file_loads_none() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        assert_eq!(store.load(DEVICE).await, None);
        assert!(!tmp.file().exists(), "load never creates the file");
    }

    #[tokio::test]
    async fn corrupt_or_non_object_reads_as_empty() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        for contents in ["not json {{{", "[1,2]", "null", "42", "\"x\""] {
            tmp.write(contents);
            assert_eq!(store.load(DEVICE).await, None, "contents: {contents}");
        }
        // No .bak, no logging — and a subsequent store overwrites cleanly.
        tmp.write("corrupt");
        store.store(DEVICE, 7).await.unwrap();
        assert_eq!(store.load(DEVICE).await, Some(7));
        assert!(!Path::new(&format!("{}.bak", tmp.file().display())).exists());
    }

    /// Self-heal input vectors (capture-stores §8 + scaffold SequenceBacking
    /// contract): non-integer numbers -> None; negative INTEGERS surface as
    /// Some(-n) for the sequencer to floor; non-numbers -> None.
    #[tokio::test]
    async fn invalid_stored_values_map_per_contract() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        let cases: &[(&str, Option<i64>)] = &[
            ("{\"d\": 41.5}", None),     // non-integer number
            ("{\"d\": 1e300}", None),    // out of integer range
            ("{\"d\": -42}", Some(-42)), // negative integer passes through
            ("{\"d\": 0}", Some(0)),
            ("{\"d\": \"42\"}", None), // string is not a number
            ("{\"d\": null}", None),
            ("{\"d\": true}", None),
            ("{\"d\": [1]}", None),
            ("{\"d\": 9007199254740991}", Some(9_007_199_254_740_991)), // 2^53-1
        ];
        for (contents, expected) in cases {
            tmp.write(contents);
            assert_eq!(store.load("d").await, *expected, "contents: {contents}");
        }
    }

    #[tokio::test]
    async fn store_preserves_other_devices() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.store("a", 1).await.unwrap();
        store.store("b", 2).await.unwrap();
        store.store("a", 5).await.unwrap();
        assert_eq!(store.load("a").await, Some(5));
        assert_eq!(store.load("b").await, Some(2));
    }

    #[tokio::test]
    async fn remove_deletes_key_and_keeps_empty_object_file() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.store("a", 1).await.unwrap();
        store.store("b", 2).await.unwrap();

        store.remove("a").await.unwrap();
        assert_eq!(store.load("a").await, None);
        assert_eq!(store.load("b").await, Some(2));

        store.remove("b").await.unwrap();
        // Contrast with the DPAPI store: the file stays as `{}`.
        assert_eq!(fs::read(tmp.file()).unwrap(), b"{}");
    }

    #[tokio::test]
    async fn remove_of_missing_key_touches_nothing() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        store.remove(DEVICE).await.unwrap();
        assert!(
            !tmp.file().exists(),
            "remove only writes when the key existed"
        );

        store.store("other", 3).await.unwrap();
        let before = fs::metadata(tmp.file()).unwrap().modified().unwrap();
        store.remove(DEVICE).await.unwrap();
        let after = fs::metadata(tmp.file()).unwrap().modified().unwrap();
        assert_eq!(before, after, "no rewrite for a missing key");
        assert_eq!(store.load("other").await, Some(3));
    }

    #[tokio::test]
    async fn backing_trait_dispatch_matches_inherent_methods() {
        let tmp = TempDir::new();
        let store = store_in(&tmp);
        let backing: &dyn SequenceBacking = &store;
        backing.store(DEVICE, 9).await.unwrap();
        assert_eq!(backing.load(DEVICE).await, Some(9));
    }
}
