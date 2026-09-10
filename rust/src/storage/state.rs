use serde::{Deserialize, Serialize};
use std::{fmt, io};

/// Logical stores covered by the reliability storage layer.
///
/// These identifiers are safe to include in diagnostics. They deliberately do
/// not contain filesystem paths, account identifiers, or credential material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreKind {
    Unspecified,
    Settings,
    ApiKeys,
    ManualCookies,
    TokenAccounts,
    KimiAccounts,
    Hooks,
    WindowGeometry,
    ProviderLastGood,
    MigrationJournal,
}

impl StoreKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Settings => "settings",
            Self::ApiKeys => "api-keys",
            Self::ManualCookies => "manual-cookies",
            Self::TokenAccounts => "token-accounts",
            Self::KimiAccounts => "kimi-accounts",
            Self::Hooks => "hooks",
            Self::WindowGeometry => "window-geometry",
            Self::ProviderLastGood => "provider-last-good",
            Self::MigrationJournal => "migration-journal",
        }
    }
}

impl fmt::Display for StoreKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A path-free name for the storage step that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreOperation {
    Load,
    Lock,
    Read,
    ReadEnvelope,
    Decrypt,
    ParsePayload,
    ValidateVersion,
    Serialize,
    Protect,
    WriteTemporary,
    SyncTemporary,
    Backup,
    Replace,
    ReadBack,
    Verify,
    SyncParent,
    ApplyPermissions,
    Migrate,
}

impl StoreOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Lock => "lock",
            Self::Read => "read",
            Self::ReadEnvelope => "read-envelope",
            Self::Decrypt => "decrypt",
            Self::ParsePayload => "parse-payload",
            Self::ValidateVersion => "validate-version",
            Self::Serialize => "serialize",
            Self::Protect => "protect",
            Self::WriteTemporary => "write-temporary",
            Self::SyncTemporary => "sync-temporary",
            Self::Backup => "backup",
            Self::Replace => "replace",
            Self::ReadBack => "read-back",
            Self::Verify => "verify",
            Self::SyncParent => "sync-parent",
            Self::ApplyPermissions => "apply-permissions",
            Self::Migrate => "migrate",
        }
    }
}

impl fmt::Display for StoreOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A redacted failure classification suitable for logs and IPC responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreFailureKind {
    NotFound,
    PermissionDenied,
    AuthenticationFailed,
    InvalidData,
    UnexpectedEof,
    TimedOut,
    WouldBlock,
    AlreadyExists,
    WriteZero,
    Interrupted,
    DiskFull,
    ResourceBusy,
    UnsupportedProtection,
    Other,
}

impl StoreFailureKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::PermissionDenied => "permission-denied",
            Self::AuthenticationFailed => "authentication-failed",
            Self::InvalidData => "invalid-data",
            Self::UnexpectedEof => "unexpected-eof",
            Self::TimedOut => "timed-out",
            Self::WouldBlock => "would-block",
            Self::AlreadyExists => "already-exists",
            Self::WriteZero => "write-zero",
            Self::Interrupted => "interrupted",
            Self::DiskFull => "disk-full",
            Self::ResourceBusy => "resource-busy",
            Self::UnsupportedProtection => "unsupported-protection",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for StoreFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Redacted OS failure details. The source message is intentionally discarded
/// because it may contain a path, payload fragment, or credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreFailure {
    pub kind: StoreFailureKind,
    pub os_code: Option<i32>,
}

impl StoreFailure {
    pub const fn new(kind: StoreFailureKind, os_code: Option<i32>) -> Self {
        Self { kind, os_code }
    }

    pub fn from_io(source: &io::Error) -> Self {
        let os_code = source.raw_os_error();
        let kind = match os_code {
            Some(28) | Some(39) | Some(112) => StoreFailureKind::DiskFull,
            Some(16) | Some(32) | Some(33) => StoreFailureKind::ResourceBusy,
            _ => match source.kind() {
                io::ErrorKind::NotFound => StoreFailureKind::NotFound,
                io::ErrorKind::PermissionDenied => StoreFailureKind::PermissionDenied,
                io::ErrorKind::AlreadyExists => StoreFailureKind::AlreadyExists,
                io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => {
                    StoreFailureKind::InvalidData
                }
                io::ErrorKind::TimedOut => StoreFailureKind::TimedOut,
                io::ErrorKind::WouldBlock => StoreFailureKind::WouldBlock,
                io::ErrorKind::WriteZero => StoreFailureKind::WriteZero,
                io::ErrorKind::Interrupted => StoreFailureKind::Interrupted,
                io::ErrorKind::UnexpectedEof => StoreFailureKind::UnexpectedEof,
                io::ErrorKind::Unsupported => StoreFailureKind::UnsupportedProtection,
                _ => StoreFailureKind::Other,
            },
        };

        Self { kind, os_code }
    }
}

impl fmt::Display for StoreFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.os_code {
            Some(code) => write!(formatter, "{} (os-code={code})", self.kind),
            None => self.kind.fmt(formatter),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadStateKind {
    Missing,
    Loaded,
    NeedsMigration,
    CorruptEnvelope,
    DecryptFailed,
    InvalidPayload,
    UnsupportedVersion,
    Locked,
    IoError,
}

/// Result of loading a store without collapsing failures into an empty value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState<T> {
    Missing,
    Loaded(T),
    NeedsMigration {
        value: T,
        source_version: u32,
    },
    CorruptEnvelope {
        reason: StoreFailure,
    },
    DecryptFailed {
        reason: StoreFailure,
    },
    InvalidPayload {
        reason: StoreFailure,
    },
    UnsupportedVersion {
        found: u32,
        supported: u32,
    },
    Locked {
        reason: StoreFailure,
    },
    IoError {
        operation: StoreOperation,
        reason: StoreFailure,
    },
}

impl<T> LoadState<T> {
    pub const fn kind(&self) -> LoadStateKind {
        match self {
            Self::Missing => LoadStateKind::Missing,
            Self::Loaded(_) => LoadStateKind::Loaded,
            Self::NeedsMigration { .. } => LoadStateKind::NeedsMigration,
            Self::CorruptEnvelope { .. } => LoadStateKind::CorruptEnvelope,
            Self::DecryptFailed { .. } => LoadStateKind::DecryptFailed,
            Self::InvalidPayload { .. } => LoadStateKind::InvalidPayload,
            Self::UnsupportedVersion { .. } => LoadStateKind::UnsupportedVersion,
            Self::Locked { .. } => LoadStateKind::Locked,
            Self::IoError { .. } => LoadStateKind::IoError,
        }
    }

    pub const fn can_write(&self) -> bool {
        matches!(self, Self::Missing | Self::Loaded(_))
    }

    pub const fn requires_migration(&self) -> bool {
        matches!(self, Self::NeedsMigration { .. })
    }

    pub const fn requires_recovery(&self) -> bool {
        matches!(
            self,
            Self::CorruptEnvelope { .. }
                | Self::DecryptFailed { .. }
                | Self::InvalidPayload { .. }
                | Self::UnsupportedVersion { .. }
                | Self::Locked { .. }
                | Self::IoError { .. }
        )
    }
}

impl<T: Default> LoadState<T> {
    pub fn into_writable(self) -> Result<T, StoreError> {
        self.into_writable_for(StoreKind::Unspecified)
    }

    pub fn into_writable_for(self, store: StoreKind) -> Result<T, StoreError> {
        match self {
            Self::Missing => Ok(T::default()),
            Self::Loaded(value) => Ok(value),
            Self::NeedsMigration { source_version, .. } => Err(StoreError::MigrationRequired {
                store,
                source_version,
            }),
            Self::CorruptEnvelope { reason } => Err(StoreError::CorruptEnvelope { store, reason }),
            Self::DecryptFailed { reason } => Err(StoreError::DecryptFailed { store, reason }),
            Self::InvalidPayload { reason } => Err(StoreError::InvalidPayload { store, reason }),
            Self::UnsupportedVersion { found, supported } => Err(StoreError::UnsupportedVersion {
                store,
                found,
                supported,
            }),
            Self::Locked { reason } => Err(StoreError::Locked { store, reason }),
            Self::IoError { operation, reason } => Err(StoreError::Io {
                store,
                operation,
                reason,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreMutationFailureKind {
    Duplicate,
    NotFound,
    InvalidValue,
    InvalidOrder,
    OrdinalExhausted,
    ActiveLimitReached,
}

impl fmt::Display for StoreMutationFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Duplicate => "duplicate",
            Self::NotFound => "not-found",
            Self::InvalidValue => "invalid-value",
            Self::InvalidOrder => "invalid-order",
            Self::OrdinalExhausted => "ordinal-exhausted",
            Self::ActiveLimitReached => "active-limit-reached",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    MigrationRequired {
        store: StoreKind,
        source_version: u32,
    },
    CorruptEnvelope {
        store: StoreKind,
        reason: StoreFailure,
    },
    DecryptFailed {
        store: StoreKind,
        reason: StoreFailure,
    },
    InvalidPayload {
        store: StoreKind,
        reason: StoreFailure,
    },
    UnsupportedVersion {
        store: StoreKind,
        found: u32,
        supported: u32,
    },
    Locked {
        store: StoreKind,
        reason: StoreFailure,
    },
    LockTimeout {
        store: StoreKind,
        reason: StoreFailure,
    },
    MutationRejected {
        store: StoreKind,
        reason: StoreMutationFailureKind,
        limit: Option<usize>,
    },
    Io {
        store: StoreKind,
        operation: StoreOperation,
        reason: StoreFailure,
    },
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MigrationRequired {
                store,
                source_version,
            } => write!(
                formatter,
                "store {store} requires migration from version {source_version}"
            ),
            Self::CorruptEnvelope { store, reason } => {
                write!(formatter, "store {store} has a corrupt envelope: {reason}")
            }
            Self::DecryptFailed { store, reason } => {
                write!(formatter, "store {store} could not be decrypted: {reason}")
            }
            Self::InvalidPayload { store, reason } => {
                write!(formatter, "store {store} has an invalid payload: {reason}")
            }
            Self::UnsupportedVersion {
                store,
                found,
                supported,
            } => write!(
                formatter,
                "store {store} has unsupported version {found}; supported version is {supported}"
            ),
            Self::Locked { store, reason } => {
                write!(formatter, "store {store} is locked: {reason}")
            }
            Self::LockTimeout { store, reason } => {
                write!(formatter, "store {store} lock timed out: {reason}")
            }
            Self::MutationRejected {
                store,
                reason,
                limit,
            } => match limit {
                Some(limit) => write!(
                    formatter,
                    "store {store} rejected mutation: {reason} (limit={limit})"
                ),
                None => write!(formatter, "store {store} rejected mutation: {reason}"),
            },
            Self::Io {
                store,
                operation,
                reason,
            } => write!(
                formatter,
                "store {store} failed during {operation}: {reason}"
            ),
        }
    }
}

impl std::error::Error for StoreError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct Fixture {
        count: u32,
    }

    fn failure(kind: StoreFailureKind) -> StoreFailure {
        StoreFailure::new(kind, Some(5))
    }

    #[test]
    fn load_states_keep_missing_loaded_legacy_and_failures_distinct() {
        let cases = vec![
            (
                "missing",
                LoadState::<Fixture>::Missing,
                LoadStateKind::Missing,
                true,
                false,
                false,
            ),
            (
                "loaded",
                LoadState::Loaded(Fixture { count: 1 }),
                LoadStateKind::Loaded,
                true,
                false,
                false,
            ),
            (
                "legacy",
                LoadState::NeedsMigration {
                    value: Fixture { count: 2 },
                    source_version: 1,
                },
                LoadStateKind::NeedsMigration,
                false,
                false,
                true,
            ),
            (
                "corrupt",
                LoadState::CorruptEnvelope {
                    reason: failure(StoreFailureKind::InvalidData),
                },
                LoadStateKind::CorruptEnvelope,
                false,
                true,
                false,
            ),
            (
                "decrypt-failed",
                LoadState::DecryptFailed {
                    reason: failure(StoreFailureKind::AuthenticationFailed),
                },
                LoadStateKind::DecryptFailed,
                false,
                true,
                false,
            ),
            (
                "future-version",
                LoadState::UnsupportedVersion {
                    found: 3,
                    supported: 2,
                },
                LoadStateKind::UnsupportedVersion,
                false,
                true,
                false,
            ),
            (
                "permission-denied",
                LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: failure(StoreFailureKind::PermissionDenied),
                },
                LoadStateKind::IoError,
                false,
                true,
                false,
            ),
        ];

        for (name, state, kind, can_write, requires_recovery, requires_migration) in cases {
            assert_eq!(state.kind(), kind, "{name}");
            assert_eq!(state.can_write(), can_write, "{name}");
            assert_eq!(state.requires_recovery(), requires_recovery, "{name}");
            assert_eq!(state.requires_migration(), requires_migration, "{name}");
        }
    }

    #[test]
    fn only_missing_and_loaded_enter_an_ordinary_write_transaction() {
        assert_eq!(
            LoadState::<Fixture>::Missing
                .into_writable_for(StoreKind::ApiKeys)
                .unwrap(),
            Fixture::default()
        );
        assert_eq!(
            LoadState::Loaded(Fixture { count: 7 })
                .into_writable_for(StoreKind::ApiKeys)
                .unwrap(),
            Fixture { count: 7 }
        );

        let blocked = vec![
            LoadState::NeedsMigration {
                value: Fixture { count: 9 },
                source_version: 1,
            },
            LoadState::CorruptEnvelope {
                reason: failure(StoreFailureKind::InvalidData),
            },
            LoadState::DecryptFailed {
                reason: failure(StoreFailureKind::AuthenticationFailed),
            },
            LoadState::InvalidPayload {
                reason: failure(StoreFailureKind::InvalidData),
            },
            LoadState::UnsupportedVersion {
                found: 3,
                supported: 2,
            },
            LoadState::Locked {
                reason: failure(StoreFailureKind::TimedOut),
            },
            LoadState::IoError {
                operation: StoreOperation::Read,
                reason: failure(StoreFailureKind::PermissionDenied),
            },
        ];

        for state in blocked {
            assert!(
                state.into_writable_for(StoreKind::ApiKeys).is_err(),
                "blocked state must never become an empty writable value"
            );
        }
    }

    #[test]
    fn migration_error_preserves_source_version_without_payload() {
        let error = LoadState::NeedsMigration {
            value: Fixture { count: 42 },
            source_version: 1,
        }
        .into_writable_for(StoreKind::ApiKeys)
        .unwrap_err();

        assert_eq!(
            error,
            StoreError::MigrationRequired {
                store: StoreKind::ApiKeys,
                source_version: 1,
            }
        );
        assert!(!error.to_string().contains("42"));
    }

    #[test]
    fn io_failure_discards_sensitive_source_message() {
        let fixture_secret = "fixture-secret-never-print";
        let source = io::Error::new(io::ErrorKind::PermissionDenied, fixture_secret);
        let failure = StoreFailure::from_io(&source);
        let error = StoreError::Io {
            store: StoreKind::ApiKeys,
            operation: StoreOperation::Read,
            reason: failure.clone(),
        };

        assert_eq!(failure.kind, StoreFailureKind::PermissionDenied);
        assert_eq!(failure.os_code, None);
        assert!(!format!("{failure:?}").contains(fixture_secret));
        assert!(!error.to_string().contains(fixture_secret));
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains(fixture_secret)
        );
    }

    #[test]
    fn mutation_rejection_reports_only_safe_structured_details() {
        let fixture_secret = "fixture-secret-never-print";
        let error = StoreError::MutationRejected {
            store: StoreKind::ApiKeys,
            reason: StoreMutationFailureKind::ActiveLimitReached,
            limit: Some(64),
        };

        let rendered = error.to_string();
        assert!(rendered.contains("api-keys"));
        assert!(rendered.contains("active-limit-reached"));
        assert!(rendered.contains("64"));
        assert!(!rendered.contains(fixture_secret));
        assert!(!format!("{error:?}").contains(fixture_secret));
    }

    #[test]
    fn unspecified_convenience_conversion_is_still_fail_closed() {
        assert_eq!(
            LoadState::<Fixture>::Missing.into_writable().unwrap(),
            Fixture::default()
        );
        assert!(
            LoadState::<Fixture>::InvalidPayload {
                reason: failure(StoreFailureKind::InvalidData),
            }
            .into_writable()
            .is_err()
        );
    }
}
