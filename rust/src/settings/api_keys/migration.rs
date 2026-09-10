use super::{
    API_KEYS_MUTATION_LOCK, API_KEYS_STORE_VERSION, ApiKeyEntry, ApiKeyInactiveReason,
    ApiKeyMutationError, ApiKeyProviderEntries, ApiKeys, MAX_ACTIVE_API_KEYS_PER_PROVIDER,
    current_saved_at,
};
use crate::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, default_protection,
    protected_file_bytes_with, restrict_storage_file,
};
use crate::storage::{
    CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest, Protection,
    RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
    StoreMutationFailureKind, StoreOperation, UserScopeId, write_bytes_verified_for,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";

#[derive(Clone)]
pub struct ApiKeyRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for ApiKeyRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiKeyRepository")
            .field("store", &StoreKind::ApiKeys)
            .field("protection", &self.protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl ApiKeyRepository {
    pub fn discover() -> Result<Self, StoreError> {
        Ok(Self::new(
            StoragePaths::discover()?,
            UserScopeId::current()?,
        ))
    }

    pub fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self::with_protection(
            paths,
            user_scope,
            default_protection(),
            Arc::new(PlatformSecretProtector),
        )
    }

    pub fn with_protection(
        paths: StoragePaths,
        user_scope: UserScopeId,
        protection: Protection,
        protector: Arc<dyn SecretProtector>,
    ) -> Self {
        Self {
            paths,
            user_scope,
            protection,
            protector,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.api_keys()
    }

    pub fn load(&self) -> Result<LoadState<ApiKeys>, StoreError> {
        let _process_guard = API_KEYS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    pub fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut ApiKeys) -> Result<R, ApiKeyMutationError>,
    ) -> Result<R, StoreError> {
        let _process_guard = API_KEYS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;

        let initial = self.load_or_migrate_unlocked()?;
        ensure_writable_state(initial)?;

        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::ApiKeys,
            &self.user_scope,
            self.paths.api_keys(),
            self.lock_timeout,
        )?;
        let mut keys = self
            .load_v2_unlocked()
            .into_writable_for(StoreKind::ApiKeys)?;
        let result = mutation(&mut keys).map_err(map_mutation_error)?;
        validate_v2(&keys)?;

        let protected = self.protected_bytes(&keys)?;
        write_bytes_verified_for(
            StoreKind::ApiKeys,
            self.paths.api_keys(),
            &protected,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.api_keys()).map_err(|source| StoreError::Io {
            store: StoreKind::ApiKeys,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })?;

        match self.load_v2_unlocked() {
            LoadState::Loaded(read_back) if read_back == keys => Ok(result),
            LoadState::Loaded(_) => Err(invalid_payload_error()),
            state => Err(load_state_error(state)),
        }
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<ApiKeys>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.api_keys().exists() {
            return Ok(self.load_v2_unlocked());
        }

        let LegacyDiscovery { sources, failure } = self.discover_legacy_sources()?;
        if let Some(failure) = failure {
            return Ok(failure);
        }
        if sources.is_empty() {
            return Ok(LoadState::Missing);
        }

        let merged = merge_legacy_sources(&sources)?;
        validate_v2(&merged)?;
        let destination_bytes = self.protected_bytes(&merged)?;
        let primary = sources
            .iter()
            .find(|source| source.primary)
            .unwrap_or(&sources[0]);
        let request = MigrationRequest {
            store: StoreKind::ApiKeys,
            source_path: primary.path.clone(),
            source_version: primary.version,
            destination_path: self.paths.api_keys().to_path_buf(),
            destination_bytes,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_v2_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_v2_unlocked())
    }

    fn load_v2_unlocked(&self) -> LoadState<ApiKeys> {
        let raw = match fs::read(self.paths.api_keys()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };

        match self.decode_v2_bytes(&raw) {
            Ok(keys) => LoadState::Loaded(keys),
            Err(StoreError::CorruptEnvelope { reason, .. }) => {
                LoadState::CorruptEnvelope { reason }
            }
            Err(StoreError::DecryptFailed { reason, .. }) => LoadState::DecryptFailed { reason },
            Err(StoreError::InvalidPayload { reason, .. }) => LoadState::InvalidPayload { reason },
            Err(StoreError::UnsupportedVersion {
                found, supported, ..
            }) => LoadState::UnsupportedVersion { found, supported },
            Err(StoreError::LockTimeout { reason, .. } | StoreError::Locked { reason, .. }) => {
                LoadState::Locked { reason }
            }
            Err(StoreError::Io {
                operation, reason, ..
            }) => LoadState::IoError { operation, reason },
            Err(StoreError::MigrationRequired { .. } | StoreError::MutationRejected { .. }) => {
                LoadState::InvalidPayload {
                    reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                }
            }
        }
    }

    fn decode_v2_bytes(&self, raw: &[u8]) -> Result<ApiKeys, StoreError> {
        let secure_envelope = raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::ApiKeys,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::ApiKeys,
                        reason,
                    }
                }
            } else {
                StoreError::InvalidPayload {
                    store: StoreKind::ApiKeys,
                    reason: StoreFailure::from_io(&source),
                }
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::ApiKeys,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        let value = serde_json::from_str::<Value>(&payload).map_err(|_| invalid_payload_error())?;
        let found = value
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(invalid_payload_error)?;
        if found != API_KEYS_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::ApiKeys,
                found,
                supported: API_KEYS_STORE_VERSION,
            });
        }
        let keys = serde_json::from_value::<ApiKeys>(value).map_err(|_| invalid_payload_error())?;
        validate_v2(&keys)?;
        Ok(keys)
    }

    fn protected_bytes(&self, keys: &ApiKeys) -> Result<Vec<u8>, StoreError> {
        let json = serde_json::to_string(keys).map_err(|_| StoreError::Io {
            store: StoreKind::ApiKeys,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        protected_file_bytes_with(&json, self.protection, self.protector.as_ref()).map_err(
            |source| StoreError::Io {
                store: StoreKind::ApiKeys,
                operation: StoreOperation::Protect,
                reason: StoreFailure::from_io(&source),
            },
        )
    }

    fn discover_legacy_sources(&self) -> Result<LegacyDiscovery, StoreError> {
        let current = self.paths.legacy_api_keys();
        let mut sources = Vec::new();
        let mut failure = None;
        if current.exists() {
            match self.read_legacy_source(&current, true) {
                Ok(source) => sources.push(source),
                Err(state) => failure = Some(state),
            }
        }

        let directory_entries =
            fs::read_dir(self.paths.migration_backups()).map_err(|source| StoreError::Io {
                store: StoreKind::ApiKeys,
                operation: StoreOperation::Read,
                reason: StoreFailure::from_io(&source),
            })?;
        let mut backups = Vec::new();
        for entry in directory_entries {
            let entry = entry.map_err(|source| StoreError::Io {
                store: StoreKind::ApiKeys,
                operation: StoreOperation::Read,
                reason: StoreFailure::from_io(&source),
            })?;
            let path = entry.path();
            let is_api_key_backup = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("api-keys-") && name.ends_with(".backup"));
            if is_api_key_backup {
                backups.push(path);
            }
        }
        backups.sort();
        for path in backups {
            let raw = fs::read(&path).map_err(|source| StoreError::Io {
                store: StoreKind::ApiKeys,
                operation: StoreOperation::Read,
                reason: StoreFailure::from_io(&source),
            })?;
            if !verified_legacy_backup_name(&path, &raw) {
                continue;
            }
            match self.parse_legacy_source(path, raw, false) {
                Ok(source) => sources.push(source),
                Err(state) if failure.is_none() => failure = Some(state),
                Err(_) => {}
            }
        }

        sources.sort_by(|left, right| {
            right
                .version
                .cmp(&left.version)
                .then_with(|| right.primary.cmp(&left.primary))
                .then_with(|| left.path.file_name().cmp(&right.path.file_name()))
        });
        Ok(LegacyDiscovery { sources, failure })
    }

    fn read_legacy_source(
        &self,
        path: &Path,
        primary: bool,
    ) -> Result<LegacySource, LoadState<ApiKeys>> {
        let raw = fs::read(path).map_err(|source| LoadState::IoError {
            operation: StoreOperation::Read,
            reason: StoreFailure::from_io(&source),
        })?;
        self.parse_legacy_source(path.to_path_buf(), raw, primary)
    }

    fn parse_legacy_source(
        &self,
        path: PathBuf,
        raw: Vec<u8>,
        primary: bool,
    ) -> Result<LegacySource, LoadState<ApiKeys>> {
        let secure_envelope = raw_is_secure_envelope(&raw);
        let payload =
            decode_string_bytes_with(&raw, self.protector.as_ref()).map_err(|source| {
                if secure_envelope {
                    if source.kind() == io::ErrorKind::InvalidData {
                        LoadState::CorruptEnvelope {
                            reason: StoreFailure::from_io(&source),
                        }
                    } else {
                        LoadState::DecryptFailed {
                            reason: if source.kind() == io::ErrorKind::Unsupported {
                                StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                            } else {
                                StoreFailure::from_io(&source)
                            },
                        }
                    }
                } else {
                    LoadState::InvalidPayload {
                        reason: StoreFailure::from_io(&source),
                    }
                }
            })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(LoadState::CorruptEnvelope {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        let value =
            serde_json::from_str::<Value>(&payload).map_err(|_| LoadState::InvalidPayload {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            })?;
        let version = value.get("version").and_then(Value::as_u64);
        let file =
            match version {
                Some(1) => LegacyFile::V1(serde_json::from_value::<LegacyV1Store>(value).map_err(
                    |_| LoadState::InvalidPayload {
                        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                    },
                )?),
                Some(found) => {
                    return Err(LoadState::UnsupportedVersion {
                        found: u32::try_from(found).unwrap_or(u32::MAX),
                        supported: 1,
                    });
                }
                None => LegacyFile::V0(serde_json::from_value::<LegacyV0Store>(value).map_err(
                    |_| LoadState::InvalidPayload {
                        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
                    },
                )?),
            };
        Ok(LegacySource {
            path,
            version: file.version(),
            file,
            primary,
        })
    }
}

#[derive(Debug)]
struct LegacyDiscovery {
    sources: Vec<LegacySource>,
    failure: Option<LoadState<ApiKeys>>,
}

#[derive(Debug)]
struct LegacySource {
    path: PathBuf,
    version: u32,
    file: LegacyFile,
    primary: bool,
}

#[derive(Debug)]
enum LegacyFile {
    V0(LegacyV0Store),
    V1(LegacyV1Store),
}

impl LegacyFile {
    const fn version(&self) -> u32 {
        match self {
            Self::V0(_) => 0,
            Self::V1(_) => 1,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV0Store {
    keys: HashMap<String, LegacyV0Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LegacyV0Value {
    Secret(String),
    Entry(LegacyV0Entry),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV0Entry {
    #[serde(alias = "apiKey")]
    api_key: String,
    #[serde(default)]
    saved_at: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Store {
    version: u32,
    providers: HashMap<String, LegacyV1Provider>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Provider {
    next_ordinal: u64,
    entries: Vec<LegacyV1Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1Entry {
    id: Uuid,
    secret: String,
    saved_at: String,
    label: Option<String>,
    ordinal: u64,
}

fn merge_legacy_sources(sources: &[LegacySource]) -> Result<ApiKeys, StoreError> {
    let mut merged = ApiKeys::default();
    let mut seen = HashMap::<String, HashSet<String>>::new();

    for source in sources
        .iter()
        .filter(|source| matches!(source.file, LegacyFile::V1(_)))
    {
        let LegacyFile::V1(store) = &source.file else {
            unreachable!();
        };
        if store.version != 1 {
            return Err(invalid_payload_error());
        }
        let mut provider_ids = store.providers.keys().cloned().collect::<Vec<_>>();
        provider_ids.sort();
        for provider_id in provider_ids {
            let source_provider = &store.providers[&provider_id];
            let destination = merged.providers.entry(provider_id.clone()).or_default();
            destination.next_ordinal = destination
                .next_ordinal
                .max(source_provider.next_ordinal.max(1));
            let provider_seen = seen.entry(provider_id).or_default();
            for entry in &source_provider.entries {
                if !provider_seen.insert(entry.secret.clone()) {
                    continue;
                }
                let ordinal = unique_ordinal(destination, entry.ordinal)?;
                destination.entries.push(ApiKeyEntry {
                    id: entry.id,
                    secret: entry.secret.clone(),
                    saved_at: entry.saved_at.clone(),
                    label: entry.label.clone(),
                    ordinal,
                    active: true,
                    inactive_reason: None,
                });
            }
        }
    }

    for source in sources
        .iter()
        .filter(|source| matches!(source.file, LegacyFile::V0(_)))
    {
        let LegacyFile::V0(store) = &source.file else {
            unreachable!();
        };
        let mut provider_ids = store.keys.keys().cloned().collect::<Vec<_>>();
        provider_ids.sort();
        for provider_id in provider_ids {
            let (secret, saved_at, label) = match &store.keys[&provider_id] {
                LegacyV0Value::Secret(secret) => (secret.clone(), current_saved_at(), None),
                LegacyV0Value::Entry(entry) => (
                    entry.api_key.clone(),
                    if entry.saved_at.is_empty() {
                        current_saved_at()
                    } else {
                        entry.saved_at.clone()
                    },
                    entry.label.clone(),
                ),
            };
            if !seen
                .entry(provider_id.clone())
                .or_default()
                .insert(secret.clone())
            {
                continue;
            }
            let destination = merged.providers.entry(provider_id).or_default();
            let ordinal = next_ordinal(destination)?;
            destination.entries.push(ApiKeyEntry {
                id: Uuid::new_v4(),
                secret,
                saved_at,
                label,
                ordinal,
                active: true,
                inactive_reason: None,
            });
        }
    }

    for provider in merged.providers.values_mut() {
        for (index, entry) in provider.entries.iter_mut().enumerate() {
            entry.active = index < MAX_ACTIVE_API_KEYS_PER_PROVIDER;
            entry.inactive_reason =
                (!entry.active).then_some(ApiKeyInactiveReason::OverLimitMigration);
        }
    }
    Ok(merged)
}

fn unique_ordinal(provider: &mut ApiKeyProviderEntries, requested: u64) -> Result<u64, StoreError> {
    if requested > 0
        && provider
            .entries
            .iter()
            .all(|entry| entry.ordinal != requested)
    {
        provider.next_ordinal = provider
            .next_ordinal
            .max(requested.checked_add(1).ok_or_else(invalid_payload_error)?);
        return Ok(requested);
    }
    next_ordinal(provider)
}

fn next_ordinal(provider: &mut ApiKeyProviderEntries) -> Result<u64, StoreError> {
    let ordinal = provider.next_ordinal.max(1);
    provider.next_ordinal = ordinal.checked_add(1).ok_or_else(invalid_payload_error)?;
    Ok(ordinal)
}

fn validate_v2(keys: &ApiKeys) -> Result<(), StoreError> {
    if keys.version != API_KEYS_STORE_VERSION {
        return Err(StoreError::UnsupportedVersion {
            store: StoreKind::ApiKeys,
            found: keys.version,
            supported: API_KEYS_STORE_VERSION,
        });
    }
    let mut ids = HashSet::new();
    for (provider_id, provider) in &keys.providers {
        if provider_id.is_empty()
            || provider.next_ordinal == 0
            || provider.entries.iter().filter(|entry| entry.active).count()
                > MAX_ACTIVE_API_KEYS_PER_PROVIDER
        {
            return Err(invalid_payload_error());
        }
        let mut ordinals = HashSet::new();
        let mut secrets = HashSet::new();
        let mut max_ordinal = 0_u64;
        for entry in &provider.entries {
            if entry.id.is_nil()
                || !ids.insert(entry.id)
                || entry.ordinal == 0
                || !ordinals.insert(entry.ordinal)
                || !secrets.insert(entry.secret.as_str())
                || entry.active != entry.inactive_reason.is_none()
            {
                return Err(invalid_payload_error());
            }
            max_ordinal = max_ordinal.max(entry.ordinal);
        }
        if !provider.entries.is_empty() && provider.next_ordinal <= max_ordinal {
            return Err(invalid_payload_error());
        }
    }
    Ok(())
}

fn raw_is_secure_envelope(raw: &[u8]) -> bool {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|format| format == SECURE_FILE_FORMAT)
}

#[cfg(test)]
fn verified_legacy_backup_filename(bytes: &[u8]) -> String {
    let hash = sha256(bytes);
    format!("api-keys-legacy-{}.backup", &hash[..16])
}

fn verified_legacy_backup_name(path: &Path, bytes: &[u8]) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let hash = sha256(bytes);
    name.starts_with("api-keys-") && name.ends_with(&format!("{}.backup", &hash[..16]))
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn ensure_writable_state(state: LoadState<ApiKeys>) -> Result<(), StoreError> {
    state.into_writable_for(StoreKind::ApiKeys).map(|_| ())
}

fn load_state_error(state: LoadState<ApiKeys>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::ApiKeys,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => invalid_payload_error(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::ApiKeys,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::ApiKeys,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::ApiKeys,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::ApiKeys,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::ApiKeys,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::ApiKeys,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::ApiKeys,
            operation,
            reason,
        },
    }
}

fn map_mutation_error(error: ApiKeyMutationError) -> StoreError {
    let (reason, limit) = match error {
        ApiKeyMutationError::DuplicateSecret => (StoreMutationFailureKind::Duplicate, None),
        ApiKeyMutationError::NotFound => (StoreMutationFailureKind::NotFound, None),
        ApiKeyMutationError::InvalidOrder => (StoreMutationFailureKind::InvalidOrder, None),
        ApiKeyMutationError::OrdinalExhausted => (StoreMutationFailureKind::OrdinalExhausted, None),
        ApiKeyMutationError::ActiveLimitReached { limit } => {
            (StoreMutationFailureKind::ActiveLimitReached, Some(limit))
        }
    };
    StoreError::MutationRejected {
        store: StoreKind::ApiKeys,
        reason,
        limit,
    }
}

fn invalid_payload_error() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::ApiKeys,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secure_file::{PlatformSecretProtector, SecretProtector, protected_file_bytes_with};
    use crate::storage::{LoadState, Protection, StoragePaths, UserScopeId};
    use serde_json::json;
    use std::fs;
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct IdentityProtector;

    impl SecretProtector for IdentityProtector {
        fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.to_vec())
        }

        fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Ok(encrypted.to_vec())
        }
    }

    struct FailingDecryptProtector;

    impl SecretProtector for FailingDecryptProtector {
        fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.to_vec())
        }

        fn unprotect_current_user(&self, _encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture decrypt failure",
            ))
        }
    }

    struct FailingProtectProtector;

    impl SecretProtector for FailingProtectProtector {
        fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture protection failure",
            ))
        }

        fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Ok(encrypted.to_vec())
        }
    }

    fn fixture_paths(root: &std::path::Path) -> StoragePaths {
        StoragePaths::from_roots(root.join("roaming"), root.join("local"))
    }

    fn plaintext_repository(paths: StoragePaths) -> ApiKeyRepository {
        ApiKeyRepository::with_protection(
            paths,
            UserScopeId::from_stable_identifier(b"fixture-api-key-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        )
    }

    fn loaded(repository: &ApiKeyRepository) -> ApiKeys {
        match repository.load().unwrap() {
            LoadState::Loaded(keys) => keys,
            state => panic!("expected loaded API keys, got {:?}", state.kind()),
        }
    }

    fn backup_files(paths: &StoragePaths) -> Vec<std::path::PathBuf> {
        fs::read_dir(paths.migration_backups())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "backup")
            })
            .collect()
    }

    #[test]
    fn v0_store_migrates_without_source_loss_and_keeps_a_stable_persisted_id() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_bytes = br#"{"keys":{"zai":{"api_key":"fixture-v0-secret","saved_at":"2026-01-02 03:04","label":"Legacy"},"openrouter":"fixture-v0-direct-secret"}}"#.to_vec();
        fs::write(&source, &source_bytes).unwrap();
        let repository = plaintext_repository(paths.clone());

        let first = loaded(&repository);
        let entry = &first.entries("zai")[0];
        assert_eq!(entry.secret, "fixture-v0-secret");
        assert_eq!(entry.saved_at, "2026-01-02 03:04");
        assert_eq!(entry.label.as_deref(), Some("Legacy"));
        assert_eq!(entry.ordinal, 1);
        assert!(entry.active);
        assert_eq!(entry.inactive_reason, None);
        assert_ne!(entry.id, Uuid::nil());
        let persisted_id = entry.id;
        assert_eq!(
            first.get("openrouter"),
            Some("fixture-v0-direct-secret"),
            "the earliest direct-string v0 shape must migrate too"
        );

        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        let backups = backup_files(&paths);
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), source_bytes);
        assert_eq!(loaded(&repository).entries("zai")[0].id, persisted_id);
    }

    #[test]
    fn v1_store_preserves_identity_order_labels_and_all_over_limit_entries() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let entries = (0_u128..66)
            .map(|index| {
                json!({
                    "id": Uuid::from_u128(index + 1),
                    "secret": format!("fixture-v1-secret-{index}"),
                    "saved_at": "2026-01-02 03:04",
                    "label": format!("Key {index}"),
                    "ordinal": index + 10,
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            &source,
            serde_json::to_vec(&json!({
                "version": 1,
                "providers": {
                    "openrouter": {
                        "next_ordinal": 100,
                        "entries": entries,
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let repository = plaintext_repository(paths);

        let keys = loaded(&repository);
        let migrated = keys.entries("openrouter");
        assert_eq!(migrated.len(), 66);
        assert_eq!(migrated[0].id, Uuid::from_u128(1));
        assert_eq!(migrated[0].ordinal, 10);
        assert_eq!(migrated[0].label.as_deref(), Some("Key 0"));
        assert!(migrated[..64].iter().all(|entry| entry.active));
        assert!(
            migrated[..64]
                .iter()
                .all(|entry| entry.inactive_reason.is_none())
        );
        assert!(migrated[64..].iter().all(|entry| !entry.active));
        assert!(migrated[64..].iter().all(|entry| {
            entry.inactive_reason == Some(ApiKeyInactiveReason::OverLimitMigration)
        }));
        assert_eq!(
            keys.providers["openrouter"].next_ordinal, 100,
            "v1 monotonic ordinal state must survive migration"
        );
    }

    #[test]
    fn valid_v1_backup_wins_identity_and_v0_only_secret_is_appended_once() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let current = paths.legacy_api_keys();
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        let current_bytes = br#"{"keys":{"openrouter":{"api_key":"shared-secret","saved_at":"v0"},"zai":{"api_key":"v0-only-secret","saved_at":"v0"}}}"#.to_vec();
        fs::write(&current, &current_bytes).unwrap();
        fs::create_dir_all(paths.migration_backups()).unwrap();
        let preserved_id = Uuid::from_u128(4242);
        let v1_backup = serde_json::to_vec(&json!({
            "version": 1,
            "providers": {
                "openrouter": {
                    "next_ordinal": 3,
                    "entries": [
                        {
                            "id": preserved_id,
                            "secret": "shared-secret",
                            "saved_at": "v1",
                            "label": "preferred-v1",
                            "ordinal": 1
                        },
                        {
                            "id": Uuid::from_u128(4243),
                            "secret": "v1-only-secret",
                            "saved_at": "v1",
                            "label": null,
                            "ordinal": 2
                        }
                    ]
                }
            }
        }))
        .unwrap();
        fs::write(
            paths
                .migration_backups()
                .join(verified_legacy_backup_filename(&v1_backup)),
            &v1_backup,
        )
        .unwrap();
        let repository = plaintext_repository(paths.clone());

        let keys = loaded(&repository);
        let openrouter = keys.entries("openrouter");
        assert_eq!(openrouter.len(), 2);
        assert_eq!(openrouter[0].id, preserved_id);
        assert_eq!(openrouter[0].label.as_deref(), Some("preferred-v1"));
        assert_eq!(openrouter[0].saved_at, "v1");
        assert_eq!(openrouter[1].secret, "v1-only-secret");
        assert_eq!(keys.entries("zai").len(), 1);
        assert_eq!(keys.entries("zai")[0].secret, "v0-only-secret");
        assert!(
            backup_files(&paths)
                .iter()
                .any(|path| fs::read(path).is_ok_and(|bytes| bytes == current_bytes))
        );

        let journal = fs::read_to_string(paths.migration_journal()).unwrap();
        assert!(!journal.contains("shared-secret"));
        assert!(!journal.contains("v0-only-secret"));
        assert!(!journal.contains("v1-only-secret"));
    }

    #[test]
    fn committed_v2_ignores_a_later_legacy_write() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_api_keys();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            &source,
            br#"{"keys":{"zai":{"api_key":"first-secret","saved_at":"first"}}}"#,
        )
        .unwrap();
        let repository = plaintext_repository(paths);
        assert_eq!(loaded(&repository).get("zai"), Some("first-secret"));

        fs::write(
            &source,
            br#"{"keys":{"zai":{"api_key":"old-version-later-secret","saved_at":"later"}}}"#,
        )
        .unwrap();

        assert_eq!(loaded(&repository).get("zai"), Some("first-secret"));
    }

    #[test]
    fn corrupt_decrypt_failed_and_future_v2_states_block_mutation_without_writes() {
        for scenario in ["corrupt", "corrupt-envelope", "decrypt-failed", "future"] {
            let directory = tempfile::tempdir().unwrap();
            let paths = fixture_paths(directory.path());
            paths.ensure_directories().unwrap();
            let (bytes, protection, protector): (Vec<u8>, Protection, Arc<dyn SecretProtector>) =
                match scenario {
                "corrupt" => (
                    b"not-json-fixture-secret".to_vec(),
                    Protection::Plaintext,
                    Arc::new(PlatformSecretProtector),
                ),
                "corrupt-envelope" => (
                    br#"{"format":"codexbar.secure-file","version":1,"protection":"windows-dpapi-user"}"#.to_vec(),
                    Protection::Plaintext,
                    Arc::new(PlatformSecretProtector),
                ),
                    "decrypt-failed" => (
                        protected_file_bytes_with(
                            r#"{"version":2,"providers":{}}"#,
                            Protection::CurrentUserDpapi,
                            &IdentityProtector,
                        )
                        .unwrap(),
                        Protection::CurrentUserDpapi,
                        Arc::new(FailingDecryptProtector),
                    ),
                    "future" => (
                        br#"{"version":3,"providers":{}}"#.to_vec(),
                        Protection::Plaintext,
                        Arc::new(PlatformSecretProtector),
                    ),
                    _ => unreachable!(),
                };
            fs::write(paths.api_keys(), &bytes).unwrap();
            let repository = ApiKeyRepository::with_protection(
                paths.clone(),
                UserScopeId::from_stable_identifier(scenario.as_bytes()),
                protection,
                protector,
            );
            let mutation_called = AtomicBool::new(false);

            let state = repository.load().unwrap();
            let expected_kind = match scenario {
                "corrupt" => crate::storage::LoadStateKind::InvalidPayload,
                "corrupt-envelope" => crate::storage::LoadStateKind::CorruptEnvelope,
                "decrypt-failed" => crate::storage::LoadStateKind::DecryptFailed,
                "future" => crate::storage::LoadStateKind::UnsupportedVersion,
                _ => unreachable!(),
            };
            assert_eq!(state.kind(), expected_kind, "{scenario}");

            assert!(
                repository
                    .mutate(|keys| {
                        mutation_called.store(true, Ordering::SeqCst);
                        keys.add("zai", "must-not-save", None).map(|_| ())
                    })
                    .is_err()
            );
            assert!(!mutation_called.load(Ordering::SeqCst));
            assert_eq!(fs::read(paths.api_keys()).unwrap(), bytes, "{scenario}");
        }
    }

    #[test]
    fn corrupt_verified_legacy_backup_is_never_treated_as_an_empty_store() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        paths.ensure_directories().unwrap();
        let backup_bytes = b"{invalid-legacy-backup";
        let backup = paths
            .migration_backups()
            .join(verified_legacy_backup_filename(backup_bytes));
        fs::write(&backup, backup_bytes).unwrap();
        let backup_hash = sha256(backup_bytes);
        let repository = plaintext_repository(paths.clone());
        let mutation_called = AtomicBool::new(false);

        let state = repository.load().unwrap();
        assert_eq!(state.kind(), crate::storage::LoadStateKind::InvalidPayload);
        assert!(
            repository
                .mutate(|keys| {
                    mutation_called.store(true, Ordering::SeqCst);
                    keys.add("zai", "must-not-save", None).map(|_| ())
                })
                .is_err()
        );

        assert!(!mutation_called.load(Ordering::SeqCst));
        assert!(!paths.api_keys().exists());
        assert_eq!(sha256(&fs::read(backup).unwrap()), backup_hash);
    }

    #[test]
    fn protection_failure_never_creates_the_formal_store() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let repository = ApiKeyRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"fixture-protection-failure"),
            Protection::CurrentUserDpapi,
            Arc::new(FailingProtectProtector),
        );

        assert!(
            repository
                .mutate(|keys| keys.add("zai", "must-not-persist", None).map(|_| ()))
                .is_err()
        );
        assert!(!paths.api_keys().exists());
    }

    #[test]
    fn repository_debug_output_contains_no_user_path_or_scope_identifier() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_string_lossy().to_string();
        let repository = plaintext_repository(fixture_paths(directory.path()));

        let debug = format!("{repository:?}");
        assert!(!debug.contains(&root));
        assert!(!debug.contains("fixture-api-key-user"));
    }

    #[test]
    fn repository_serializes_concurrent_mutations_without_lost_keys() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(plaintext_repository(fixture_paths(directory.path())));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();

        for index in 0..8 {
            let repository = Arc::clone(&repository);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                repository
                    .mutate(|keys| {
                        keys.add("openrouter", &format!("concurrent-secret-{index}"), None)
                            .map(|_| ())
                    })
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }

        let keys = loaded(&repository);
        assert_eq!(keys.entries("openrouter").len(), 8);
        assert_eq!(
            keys.entries("openrouter")
                .iter()
                .map(|entry| entry.ordinal)
                .collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
    }
}
