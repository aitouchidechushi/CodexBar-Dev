use std::collections::HashMap;
use std::path::{Path, PathBuf};

use leveldb_forensic::LocalStorageRecord;

#[derive(Clone, PartialEq, Eq)]
pub enum LatestLocalStorageValue {
    Present(String),
    Deleted,
    Missing,
}

#[derive(Debug, thiserror::Error)]
pub enum LocalStorageReadError {
    #[error("failed to read Chromium Local Storage at {path}: {message}")]
    ReadFailed { path: PathBuf, message: String },
}

pub fn read_latest_local_storage_values(
    leveldb_dir: &Path,
    origin: &str,
    keys: &[&str],
) -> Result<HashMap<String, LatestLocalStorageValue>, LocalStorageReadError> {
    let records = leveldb_forensic::decode_local_storage(leveldb_dir).map_err(|error| {
        LocalStorageReadError::ReadFailed {
            path: leveldb_dir.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    Ok(select_latest(records, origin, keys))
}

fn select_latest(
    records: impl IntoIterator<Item = LocalStorageRecord>,
    origin: &str,
    keys: &[&str],
) -> HashMap<String, LatestLocalStorageValue> {
    let mut selected = keys
        .iter()
        .map(|key| ((*key).to_string(), LatestLocalStorageValue::Missing))
        .collect::<HashMap<_, _>>();
    let mut latest_sequences = HashMap::<String, u64>::new();

    for record in records {
        let LocalStorageRecord::Data {
            origin: record_origin,
            script_key,
            value,
            seq,
            deleted,
        } = record
        else {
            continue;
        };
        if record_origin != origin || script_key.lossy {
            continue;
        }
        let Some(requested_key) = keys
            .iter()
            .copied()
            .find(|requested| *requested == script_key.text)
        else {
            continue;
        };
        if latest_sequences
            .get(requested_key)
            .is_some_and(|latest| *latest >= seq)
        {
            continue;
        }

        latest_sequences.insert(requested_key.to_string(), seq);
        let state = if deleted {
            LatestLocalStorageValue::Deleted
        } else if value.lossy {
            LatestLocalStorageValue::Missing
        } else {
            LatestLocalStorageValue::Present(value.text)
        };
        selected.insert(requested_key.to_string(), state);
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    use leveldb_forensic::{Encoding, LocalStorageRecord, StorageValue};

    fn value(text: &str) -> StorageValue {
        let mut raw = vec![1];
        raw.extend_from_slice(text.as_bytes());
        StorageValue {
            text: text.to_string(),
            raw,
            encoding: Encoding::Latin1,
            lossy: false,
        }
    }

    fn record(
        origin: &str,
        key: &str,
        stored_value: &str,
        seq: u64,
        deleted: bool,
    ) -> LocalStorageRecord {
        LocalStorageRecord::Data {
            origin: origin.to_string(),
            script_key: value(key),
            value: value(stored_value),
            seq,
            deleted,
        }
    }

    #[test]
    fn highest_sequence_wins_for_each_exact_origin_and_key() {
        let records = vec![
            record("https://www.kimi.com", "access_token", "old", 10, false),
            record("https://www.kimi.com", "access_token", "new", 12, false),
            record(
                "https://other.example",
                "access_token",
                "wrong-origin",
                99,
                false,
            ),
        ];

        let selected = select_latest(records, "https://www.kimi.com", &["access_token"]);

        assert!(matches!(
            selected.get("access_token"),
            Some(LatestLocalStorageValue::Present(value)) if value == "new"
        ));
    }

    #[test]
    fn newest_tombstone_never_falls_back_to_an_old_secret() {
        let records = vec![
            record("https://www.kimi.com", "refresh_token", "old", 10, false),
            record("https://www.kimi.com", "refresh_token", "", 11, true),
        ];

        let selected = select_latest(records, "https://www.kimi.com", &["refresh_token"]);

        assert!(matches!(
            selected.get("refresh_token"),
            Some(LatestLocalStorageValue::Deleted)
        ));
    }

    #[test]
    fn lossy_keys_and_unrequested_keys_are_ignored() {
        let mut lossy_key = value("access_token");
        lossy_key.lossy = true;
        let records = vec![
            LocalStorageRecord::Data {
                origin: "https://www.kimi.com".to_string(),
                script_key: lossy_key,
                value: value("must-not-survive"),
                seq: 20,
                deleted: false,
            },
            record(
                "https://www.kimi.com",
                "unrequested_key",
                "must-not-survive",
                21,
                false,
            ),
        ];

        let selected = select_latest(records, "https://www.kimi.com", &["access_token"]);

        assert!(matches!(
            selected.get("access_token"),
            Some(LatestLocalStorageValue::Missing)
        ));
        assert!(!selected.contains_key("unrequested_key"));
    }

    #[test]
    fn newest_lossy_value_never_falls_back_to_an_old_secret() {
        let mut lossy_value = value("corrupted-new-value");
        lossy_value.lossy = true;
        let records = vec![
            record("https://www.kimi.com", "access_token", "old", 10, false),
            LocalStorageRecord::Data {
                origin: "https://www.kimi.com".to_string(),
                script_key: value("access_token"),
                value: lossy_value,
                seq: 11,
                deleted: false,
            },
        ];

        let selected = select_latest(records, "https://www.kimi.com", &["access_token"]);

        assert!(matches!(
            selected.get("access_token"),
            Some(LatestLocalStorageValue::Missing)
        ));
    }

    #[test]
    fn missing_leveldb_directory_returns_a_typed_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing-leveldb");

        let error =
            read_latest_local_storage_values(&missing, "https://www.kimi.com", &["access_token"])
                .err()
                .expect("missing directory must fail");

        assert!(matches!(
            error,
            LocalStorageReadError::ReadFailed { path, .. } if path == missing
        ));
    }

    #[test]
    fn missing_requested_key_is_reported_as_missing() {
        let selected = select_latest(
            Vec::<LocalStorageRecord>::new(),
            "https://www.kimi.com",
            &["access_token"],
        );

        assert!(matches!(
            selected.get("access_token"),
            Some(LatestLocalStorageValue::Missing)
        ));
    }
}
