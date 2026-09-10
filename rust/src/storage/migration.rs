use crate::secure_file::restrict_storage_file;
use crate::storage::{
    CrossProcessStoreLock, LoadState, Protection, RealAtomicFileOps, StoragePaths, StoreError,
    StoreFailure, StoreFailureKind, StoreKind, StoreOperation, UserScopeId,
    write_bytes_verified_for, write_verified_for,
};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MIGRATION_JOURNAL_VERSION: u32 = 1;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationState {
    Started,
    BackupVerified,
    DestinationVerified,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrationResult {
    Migrated,
    ExistingDestinationPreserved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationRecord {
    pub store: StoreKind,
    pub source_path_hash: String,
    pub source_version: u32,
    /// Relative filename inside `migration-backups`; never an absolute user path.
    pub backup_path: PathBuf,
    pub destination_sha256: Option<String>,
    pub state: MigrationState,
    pub result: Option<MigrationResult>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationJournal {
    pub version: u32,
    pub records: Vec<MigrationRecord>,
}

impl Default for MigrationJournal {
    fn default() -> Self {
        Self {
            version: MIGRATION_JOURNAL_VERSION,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRequest {
    pub store: StoreKind,
    pub source_path: PathBuf,
    pub source_version: u32,
    pub destination_path: PathBuf,
    pub destination_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationCheckpoint {
    BackupPersisted,
    DestinationReplaced,
    JournalCommitted,
}

pub trait MigrationHook: Send + Sync {
    fn checkpoint(
        &self,
        _checkpoint: MigrationCheckpoint,
        _store: StoreKind,
    ) -> Result<(), StoreError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
struct NoopMigrationHook;

impl MigrationHook for NoopMigrationHook {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    Migrated,
    ExistingDestinationPreserved,
    AlreadyCompleted,
}

#[derive(Clone)]
pub struct MigrationCoordinator {
    paths: StoragePaths,
    user_scope: UserScopeId,
    hook: Arc<dyn MigrationHook>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for MigrationCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MigrationCoordinator")
            .field("paths", &self.paths)
            .field("user_scope", &self.user_scope)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl MigrationCoordinator {
    pub fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self {
            paths,
            user_scope,
            hook: Arc::new(NoopMigrationHook),
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn with_hook(mut self, hook: Arc<dyn MigrationHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn load_journal(&self) -> LoadState<MigrationJournal> {
        if let Err(error) = self.paths.ensure_directories() {
            return load_state_from_error(error);
        }
        let guard = match CrossProcessStoreLock::acquire(
            StoreKind::MigrationJournal,
            &self.user_scope,
            self.paths.migration_journal(),
            self.lock_timeout,
        ) {
            Ok(guard) => guard,
            Err(error) => return load_state_from_error(error),
        };
        let state = self.load_journal_unlocked();
        drop(guard);
        state
    }

    pub fn migrate_bytes_verified(
        &self,
        request: MigrationRequest,
        verify_destination: impl Fn(&[u8]) -> Result<(), StoreError>,
    ) -> Result<MigrationOutcome, StoreError> {
        self.paths.ensure_directories()?;
        self.validate_request(&request)?;

        let _store_guard = CrossProcessStoreLock::acquire(
            request.store,
            &self.user_scope,
            &request.destination_path,
            self.lock_timeout,
        )?;
        let _journal_guard = CrossProcessStoreLock::acquire(
            StoreKind::MigrationJournal,
            &self.user_scope,
            self.paths.migration_journal(),
            self.lock_timeout,
        )?;
        let mut journal = self
            .load_journal_unlocked()
            .into_writable_for(StoreKind::MigrationJournal)?;

        if let Some(record) = journal
            .records
            .iter()
            .find(|record| record.store == request.store)
            && record.state == MigrationState::Completed
        {
            let existing = read_bytes(
                request.store,
                StoreOperation::ReadBack,
                &request.destination_path,
            )?;
            verify_destination(&existing)?;
            return Ok(MigrationOutcome::AlreadyCompleted);
        }

        let source_path_hash = hash_source_path(&request.source_path, request.store)?;
        let source_bytes = read_bytes(request.store, StoreOperation::Read, &request.source_path)?;
        let source_sha256 = hash_bytes(&source_bytes);
        let backup_filename = format!(
            "{}-{}-{}.backup",
            request.store,
            &source_path_hash[..16],
            &source_sha256[..16]
        );
        let backup_relative = PathBuf::from(&backup_filename);
        let backup_absolute = self.paths.migration_backups().join(&backup_relative);

        let record_index = match journal
            .records
            .iter()
            .position(|record| record.store == request.store)
        {
            Some(index) => {
                let record = &journal.records[index];
                if record.source_path_hash != source_path_hash
                    || record.source_version != request.source_version
                    || record.backup_path != backup_relative
                {
                    return Err(StoreError::InvalidPayload {
                        store: request.store,
                        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                    });
                }
                index
            }
            None => {
                journal.records.push(MigrationRecord {
                    store: request.store,
                    source_path_hash,
                    source_version: request.source_version,
                    backup_path: backup_relative,
                    destination_sha256: None,
                    state: MigrationState::Started,
                    result: None,
                    started_at: timestamp_now(),
                    completed_at: None,
                });
                let index = journal.records.len() - 1;
                self.persist_journal_unlocked(&journal)?;
                index
            }
        };

        ensure_backup(
            request.store,
            &backup_absolute,
            &source_bytes,
            &source_sha256,
        )?;
        if journal.records[record_index].state == MigrationState::Started {
            journal.records[record_index].state = MigrationState::BackupVerified;
            self.persist_journal_unlocked(&journal)?;
        }
        self.hook
            .checkpoint(MigrationCheckpoint::BackupPersisted, request.store)?;

        let desired_hash = hash_bytes(&request.destination_bytes);
        let (destination_bytes, result) = if request.destination_path.exists() {
            let existing = read_bytes(
                request.store,
                StoreOperation::ReadBack,
                &request.destination_path,
            )?;
            verify_destination(&existing)?;
            let result = if hash_bytes(&existing) == desired_hash {
                MigrationResult::Migrated
            } else {
                MigrationResult::ExistingDestinationPreserved
            };
            (existing, result)
        } else {
            write_bytes_verified_for(
                request.store,
                &request.destination_path,
                &request.destination_bytes,
                &RealAtomicFileOps,
            )?;
            restrict_storage_file(&request.destination_path).map_err(|source| StoreError::Io {
                store: request.store,
                operation: StoreOperation::ApplyPermissions,
                reason: StoreFailure::from_io(&source),
            })?;
            let written = read_bytes(
                request.store,
                StoreOperation::ReadBack,
                &request.destination_path,
            )?;
            verify_destination(&written)?;
            (written, MigrationResult::Migrated)
        };
        self.hook
            .checkpoint(MigrationCheckpoint::DestinationReplaced, request.store)?;

        journal.records[record_index].destination_sha256 = Some(hash_bytes(&destination_bytes));
        journal.records[record_index].result = Some(result);
        journal.records[record_index].state = MigrationState::DestinationVerified;
        self.persist_journal_unlocked(&journal)?;

        journal.records[record_index].state = MigrationState::Completed;
        journal.records[record_index].completed_at = Some(timestamp_now());
        self.persist_journal_unlocked(&journal)?;
        self.hook
            .checkpoint(MigrationCheckpoint::JournalCommitted, request.store)?;

        Ok(match result {
            MigrationResult::Migrated => MigrationOutcome::Migrated,
            MigrationResult::ExistingDestinationPreserved => {
                MigrationOutcome::ExistingDestinationPreserved
            }
        })
    }

    fn validate_request(&self, request: &MigrationRequest) -> Result<(), StoreError> {
        if matches!(
            request.store,
            StoreKind::Unspecified | StoreKind::MigrationJournal
        ) {
            return Err(StoreError::InvalidPayload {
                store: request.store,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        let expected =
            self.paths
                .store_path(request.store)
                .ok_or_else(|| StoreError::InvalidPayload {
                    store: request.store,
                    reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                })?;
        if expected != request.destination_path || request.source_path == request.destination_path {
            return Err(StoreError::InvalidPayload {
                store: request.store,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(())
    }

    fn load_journal_unlocked(&self) -> LoadState<MigrationJournal> {
        let bytes = match std::fs::read(self.paths.migration_journal()) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        let journal = match serde_json::from_slice::<MigrationJournal>(&bytes) {
            Ok(journal) => journal,
            Err(_) => {
                return LoadState::InvalidPayload {
                    reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                };
            }
        };
        if journal.version != MIGRATION_JOURNAL_VERSION {
            return LoadState::UnsupportedVersion {
                found: journal.version,
                supported: MIGRATION_JOURNAL_VERSION,
            };
        }
        if !journal_is_valid(&journal) {
            return LoadState::InvalidPayload {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            };
        }
        LoadState::Loaded(journal)
    }

    fn persist_journal_unlocked(&self, journal: &MigrationJournal) -> Result<(), StoreError> {
        write_verified_for(
            StoreKind::MigrationJournal,
            self.paths.migration_journal(),
            journal,
            Protection::Plaintext,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.migration_journal()).map_err(|source| StoreError::Io {
            store: StoreKind::MigrationJournal,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn ensure_backup(
    store: StoreKind,
    backup: &Path,
    source_bytes: &[u8],
    source_sha256: &str,
) -> Result<(), StoreError> {
    if backup.exists() {
        let existing = read_bytes(store, StoreOperation::ReadBack, backup)?;
        if hash_bytes(&existing) != source_sha256 {
            return Err(StoreError::CorruptEnvelope {
                store,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        return Ok(());
    }

    write_bytes_verified_for(store, backup, source_bytes, &RealAtomicFileOps)?;
    restrict_storage_file(backup).map_err(|source| StoreError::Io {
        store,
        operation: StoreOperation::ApplyPermissions,
        reason: StoreFailure::from_io(&source),
    })
}

fn read_bytes(
    store: StoreKind,
    operation: StoreOperation,
    path: &Path,
) -> Result<Vec<u8>, StoreError> {
    std::fs::read(path).map_err(|source| StoreError::Io {
        store,
        operation,
        reason: StoreFailure::from_io(&source),
    })
}

fn hash_source_path(path: &Path, store: StoreKind) -> Result<String, StoreError> {
    let canonical = std::fs::canonicalize(path).map_err(|source| StoreError::Io {
        store,
        operation: StoreOperation::Read,
        reason: StoreFailure::from_io(&source),
    })?;
    Ok(hash_bytes(canonical.as_os_str().as_encoded_bytes()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn timestamp_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn journal_is_valid(journal: &MigrationJournal) -> bool {
    let mut stores = std::collections::HashSet::new();
    journal.records.iter().all(|record| {
        let hashes_are_valid = is_sha256(&record.source_path_hash)
            && record.destination_sha256.as_deref().is_none_or(is_sha256);
        let backup_is_one_relative_file = record.backup_path.is_relative()
            && record.backup_path.components().count() == 1
            && record
                .backup_path
                .extension()
                .is_some_and(|ext| ext == "backup");
        let state_is_consistent = match record.state {
            MigrationState::Started | MigrationState::BackupVerified => {
                record.destination_sha256.is_none()
                    && record.result.is_none()
                    && record.completed_at.is_none()
            }
            MigrationState::DestinationVerified => {
                record.destination_sha256.is_some()
                    && record.result.is_some()
                    && record.completed_at.is_none()
            }
            MigrationState::Completed => {
                record.destination_sha256.is_some()
                    && record.result.is_some()
                    && record.completed_at.is_some()
            }
        };
        stores.insert(record.store)
            && !matches!(
                record.store,
                StoreKind::Unspecified | StoreKind::MigrationJournal
            )
            && hashes_are_valid
            && backup_is_one_relative_file
            && state_is_consistent
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn load_state_from_error<T>(error: StoreError) -> LoadState<T> {
    match error {
        StoreError::LockTimeout { reason, .. } | StoreError::Locked { reason, .. } => {
            LoadState::Locked { reason }
        }
        StoreError::Io {
            operation, reason, ..
        } => LoadState::IoError { operation, reason },
        StoreError::CorruptEnvelope { reason, .. } => LoadState::CorruptEnvelope { reason },
        StoreError::DecryptFailed { reason, .. } => LoadState::DecryptFailed { reason },
        StoreError::InvalidPayload { reason, .. } => LoadState::InvalidPayload { reason },
        StoreError::UnsupportedVersion {
            found, supported, ..
        } => LoadState::UnsupportedVersion { found, supported },
        StoreError::MigrationRequired { .. } => LoadState::InvalidPayload {
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        },
        StoreError::MutationRejected { .. } => LoadState::InvalidPayload {
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        LoadState, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
        StoreOperation, UserScopeId,
    };
    use serde_json::Value;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FailOnceAt {
        checkpoint: MigrationCheckpoint,
        failed: AtomicBool,
    }

    impl FailOnceAt {
        fn new(checkpoint: MigrationCheckpoint) -> Self {
            Self {
                checkpoint,
                failed: AtomicBool::new(false),
            }
        }
    }

    impl MigrationHook for FailOnceAt {
        fn checkpoint(
            &self,
            checkpoint: MigrationCheckpoint,
            store: StoreKind,
        ) -> Result<(), StoreError> {
            if checkpoint == self.checkpoint && !self.failed.swap(true, Ordering::SeqCst) {
                return Err(StoreError::Io {
                    store,
                    operation: StoreOperation::Migrate,
                    reason: StoreFailure::new(StoreFailureKind::Interrupted, None),
                });
            }
            Ok(())
        }
    }

    fn fixture_paths(root: &Path) -> StoragePaths {
        StoragePaths::from_roots(root.join("roaming"), root.join("local"))
    }

    fn coordinator(paths: StoragePaths) -> MigrationCoordinator {
        MigrationCoordinator::new(
            paths,
            UserScopeId::from_stable_identifier(b"fixture-migration-user"),
        )
    }

    fn request(paths: &StoragePaths, source: &Path, destination: Vec<u8>) -> MigrationRequest {
        MigrationRequest {
            store: StoreKind::ApiKeys,
            source_path: source.to_path_buf(),
            source_version: 1,
            destination_path: paths.api_keys().to_path_buf(),
            destination_bytes: destination,
        }
    }

    fn valid_v2(bytes: &[u8]) -> Result<(), StoreError> {
        let value =
            serde_json::from_slice::<Value>(bytes).map_err(|_| StoreError::InvalidPayload {
                store: StoreKind::ApiKeys,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            })?;
        if value.get("version").and_then(Value::as_u64) != Some(2) {
            return Err(StoreError::InvalidPayload {
                store: StoreKind::ApiKeys,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(())
    }

    fn journal(coordinator: &MigrationCoordinator) -> MigrationJournal {
        match coordinator.load_journal() {
            LoadState::Loaded(journal) => journal,
            state => panic!("expected loaded journal, got {:?}", state.kind()),
        }
    }

    fn api_key_record(journal: &MigrationJournal) -> &MigrationRecord {
        journal
            .records
            .iter()
            .find(|record| record.store == StoreKind::ApiKeys)
            .unwrap()
    }

    fn backup_count(paths: &StoragePaths) -> usize {
        fs::read_dir(paths.migration_backups())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "backup"))
            .count()
    }

    #[test]
    fn malformed_journal_paths_are_rejected_without_becoming_writable_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        paths.ensure_directories().unwrap();
        fs::write(
            paths.migration_journal(),
            br#"{"version":1,"records":[{"store":"api-keys","sourcePathHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sourceVersion":1,"backupPath":"../escape.backup","destinationSha256":null,"state":"started","result":null,"startedAt":"fixture","completedAt":null}]}"#,
        )
        .unwrap();

        assert!(matches!(
            coordinator(paths).load_journal(),
            LoadState::InvalidPayload { .. }
        ));
    }

    #[test]
    fn resumes_after_backup_checkpoint_without_duplicate_backup_or_source_loss() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_bytes = br#"{"keys":{"zai":"fixture-secret"}}"#.to_vec();
        fs::write(&source, &source_bytes).unwrap();
        let destination = br#"{"version":2,"providers":{}}"#.to_vec();
        let interrupted = coordinator(paths.clone()).with_hook(Arc::new(FailOnceAt::new(
            MigrationCheckpoint::BackupPersisted,
        )));

        assert!(
            interrupted
                .migrate_bytes_verified(request(&paths, &source, destination.clone()), valid_v2)
                .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert!(!paths.api_keys().exists());
        assert_eq!(backup_count(&paths), 1);
        assert_eq!(
            api_key_record(&journal(&interrupted)).state,
            MigrationState::BackupVerified
        );

        let restarted = coordinator(paths.clone());
        assert_eq!(
            restarted
                .migrate_bytes_verified(request(&paths, &source, destination.clone()), valid_v2)
                .unwrap(),
            MigrationOutcome::Migrated
        );
        assert_eq!(fs::read(paths.api_keys()).unwrap(), destination);
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(backup_count(&paths), 1);
        let completed_journal = journal(&restarted);
        let record = api_key_record(&completed_journal);
        assert_eq!(record.state, MigrationState::Completed);
        assert!(record.completed_at.is_some());
        assert!(record.destination_sha256.is_some());
        assert!(record.backup_path.is_relative());
    }

    #[test]
    fn resumes_after_destination_replace_and_preserves_a_newer_valid_v2_value() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_bytes = br#"{"keys":{"zai":"fixture-secret"}}"#.to_vec();
        fs::write(&source, &source_bytes).unwrap();
        let intended = br#"{"version":2,"generation":1}"#.to_vec();
        let interrupted = coordinator(paths.clone()).with_hook(Arc::new(FailOnceAt::new(
            MigrationCheckpoint::DestinationReplaced,
        )));

        assert!(
            interrupted
                .migrate_bytes_verified(request(&paths, &source, intended), valid_v2)
                .is_err()
        );
        assert_eq!(
            api_key_record(&journal(&interrupted)).state,
            MigrationState::BackupVerified
        );
        let newer = br#"{"version":2,"generation":2}"#.to_vec();
        fs::write(paths.api_keys(), &newer).unwrap();

        let restarted = coordinator(paths.clone());
        assert_eq!(
            restarted
                .migrate_bytes_verified(
                    request(&paths, &source, br#"{"version":2,"generation":1}"#.to_vec(),),
                    valid_v2,
                )
                .unwrap(),
            MigrationOutcome::ExistingDestinationPreserved
        );
        assert_eq!(fs::read(paths.api_keys()).unwrap(), newer);
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        assert_eq!(backup_count(&paths), 1);
        assert_eq!(
            api_key_record(&journal(&restarted)).state,
            MigrationState::Completed
        );
    }

    #[test]
    fn committed_journal_ignores_later_legacy_writes_and_remains_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            &source,
            br#"{"keys":{"zai":"fixture-secret-journal-never-store"}}"#,
        )
        .unwrap();
        let destination = br#"{"version":2,"generation":1}"#.to_vec();
        let interrupted = coordinator(paths.clone()).with_hook(Arc::new(FailOnceAt::new(
            MigrationCheckpoint::JournalCommitted,
        )));

        assert!(
            interrupted
                .migrate_bytes_verified(request(&paths, &source, destination.clone()), valid_v2)
                .is_err()
        );
        assert_eq!(
            api_key_record(&journal(&interrupted)).state,
            MigrationState::Completed
        );
        fs::write(&source, br#"{"keys":{"zai":"old-version-later-write"}}"#).unwrap();

        let restarted = coordinator(paths.clone());
        assert_eq!(
            restarted
                .migrate_bytes_verified(
                    request(
                        &paths,
                        &source,
                        br#"{"version":2,"generation":999}"#.to_vec(),
                    ),
                    valid_v2,
                )
                .unwrap(),
            MigrationOutcome::AlreadyCompleted
        );
        assert_eq!(fs::read(paths.api_keys()).unwrap(), destination);
        assert_eq!(backup_count(&paths), 1);

        let raw_journal = fs::read_to_string(paths.migration_journal()).unwrap();
        assert!(!raw_journal.contains("fixture-secret-journal-never-store"));
        assert!(!raw_journal.contains("old-version-later-write"));
        assert!(!raw_journal.contains(&directory.path().to_string_lossy().to_string()));
        assert_eq!(journal(&restarted).records.len(), 1);
    }
}
