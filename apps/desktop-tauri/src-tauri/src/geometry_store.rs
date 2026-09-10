//! Persistent window-geometry store for the Tauri desktop shell.
//!
//! Remembers position (and size where applicable) for detached user surfaces:
//! PopOut and Settings.
//!
//! The flyout window stays computed from the tray anchor/work-area because it
//! is a temporary anchored panel, not a user-movable standalone window — but
//! its SIZE is remembered via the size-only [`StoredSize`] entries below, kept
//! separate from [`StoredGeometry`] so the flyout's persisted size can never
//! carry fabricated `x`/`y` coordinates.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::{Duration, Instant};

use codexbar::secure_file::restrict_storage_file;
use codexbar::storage::{
    AtomicFileOps, CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest,
    Protection, RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind,
    StoreKind, StoreOperation, UserScopeId, write_verified_for,
};
use serde::{Deserialize, Serialize};

use crate::surface::SurfaceMode;

/// Bumped when the meaning of stored fields changes. v1 switched the stored
/// window SIZE from physical to logical pixels, so legacy (versionless) files
/// hold physical sizes that must be discarded on load.
const GEOMETRY_VERSION: u32 = 1;
const GEOMETRY_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const GEOMETRY_WRITE_DEBOUNCE: Duration = Duration::from_millis(350);
const GEOMETRY_FLUSH_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_GEOMETRY_ENTRIES: usize = 64;
const MAX_GEOMETRY_KEY_LENGTH: usize = 128;
const MAX_LOGICAL_DIMENSION: u32 = 32_768;
static GEOMETRY_MUTATION_LOCK: Mutex<()> = Mutex::new(());

/// Persisted window geometry entry. Size is optional because not every surface
/// is resizable; we always persist position when available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredGeometry {
    pub x: i32,
    pub y: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// In-memory state owned by the single geometry writer. It keeps only the
/// newest value for each window key and suppresses a write when that value is
/// already known to be durable.
#[derive(Default)]
struct GeometryWriteCoalescer {
    pending: BTreeMap<String, StoredGeometry>,
    persisted: BTreeMap<String, StoredGeometry>,
    ready_at: Option<Instant>,
}

impl GeometryWriteCoalescer {
    fn enqueue(&mut self, key: String, geometry: StoredGeometry, now: Instant) {
        if self.persisted.get(&key) == Some(&geometry) {
            self.pending.remove(&key);
        } else {
            self.pending.insert(key, geometry);
        }
        self.ready_at = (!self.pending.is_empty()).then_some(now + GEOMETRY_WRITE_DEBOUNCE);
    }

    fn is_ready(&self, now: Instant) -> bool {
        self.ready_at.is_some_and(|ready_at| now >= ready_at)
    }

    fn wait_duration(&self, now: Instant) -> Option<Duration> {
        self.ready_at
            .map(|ready_at| ready_at.saturating_duration_since(now))
    }

    #[cfg(test)]
    fn take_ready(&mut self, now: Instant) -> Vec<(String, StoredGeometry)> {
        if self.is_ready(now) {
            self.take_all()
        } else {
            Vec::new()
        }
    }

    fn take_all(&mut self) -> Vec<(String, StoredGeometry)> {
        self.ready_at = None;
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    fn acknowledge(&mut self, key: &str, geometry: StoredGeometry) {
        self.persisted.insert(key.to_string(), geometry);
    }
}

enum GeometryWriteCommand {
    Enqueue {
        key: String,
        geometry: StoredGeometry,
    },
    Flush {
        acknowledgement: mpsc::SyncSender<()>,
    },
}

struct GeometrySaveQueue {
    sender: mpsc::Sender<GeometryWriteCommand>,
}

impl GeometrySaveQueue {
    fn start() -> Self {
        let (sender, receiver) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("codexbar-geometry-save".to_string())
            .spawn(move || run_geometry_write_worker(receiver));
        Self { sender }
    }

    fn enqueue(&self, key: String, geometry: StoredGeometry) {
        if self
            .sender
            .send(GeometryWriteCommand::Enqueue { key, geometry })
            .is_err()
        {
            tracing::warn!(
                target: "codexbar::geometry",
                store = %StoreKind::WindowGeometry,
                "geometry writer is unavailable; deferred window geometry was not persisted"
            );
        }
    }

    fn flush(&self) -> bool {
        let (acknowledgement, receiver) = mpsc::sync_channel(0);
        if self
            .sender
            .send(GeometryWriteCommand::Flush { acknowledgement })
            .is_err()
        {
            return false;
        }
        receiver.recv_timeout(GEOMETRY_FLUSH_TIMEOUT).is_ok()
    }
}

static GEOMETRY_SAVE_QUEUE: LazyLock<GeometrySaveQueue> = LazyLock::new(GeometrySaveQueue::start);

fn run_geometry_write_worker(receiver: mpsc::Receiver<GeometryWriteCommand>) {
    let mut coalescer = GeometryWriteCoalescer::default();
    loop {
        let now = Instant::now();
        if coalescer.is_ready(now) {
            flush_geometry_writes(&mut coalescer);
            continue;
        }

        let command = match coalescer.wait_duration(now) {
            Some(delay) if delay.is_zero() => {
                flush_geometry_writes(&mut coalescer);
                continue;
            }
            Some(delay) => match receiver.recv_timeout(delay) {
                Ok(command) => Some(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    flush_geometry_writes(&mut coalescer);
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            },
            None => receiver.recv().ok(),
        };

        let Some(command) = command else {
            flush_geometry_writes(&mut coalescer);
            break;
        };
        match command {
            GeometryWriteCommand::Enqueue { key, geometry } => {
                coalescer.enqueue(key, geometry, Instant::now());
            }
            GeometryWriteCommand::Flush { acknowledgement } => {
                flush_geometry_writes(&mut coalescer);
                let _ = acknowledgement.send(());
            }
        }
    }
}

fn flush_geometry_writes(coalescer: &mut GeometryWriteCoalescer) {
    for (key, geometry) in coalescer.take_all() {
        if persist_geometry_entry(&key, geometry).is_ok() {
            coalescer.acknowledge(&key, geometry);
        }
    }
}

/// A size-only persisted entry, for windows that are always re-anchored (never
/// remember position) so storing `x`/`y` would be fabricated data. Used by the
/// detached flyout window, which is anchored above the tray on every open —
/// only its user-chosen width/height is meaningful to remember.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSize {
    pub width: u32,
    pub height: u32,
}

/// All persisted geometries keyed by surface mode string (`settings`, ...).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeometryFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: std::collections::BTreeMap<String, StoredGeometry>,
    /// Size-only entries (no position), keyed by an arbitrary label (e.g. the
    /// `"flyout"` window). Kept in a separate map — rather than reusing
    /// `entries` with a fabricated `x: 0, y: 0` — so the on-disk shape can't
    /// be misread as a remembered position.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub size_entries: std::collections::BTreeMap<String, StoredSize>,
}

#[derive(Clone)]
struct GeometryRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    file_ops: Arc<dyn AtomicFileOps>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for GeometryRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeometryRepository")
            .field("store", &StoreKind::WindowGeometry)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl GeometryRepository {
    fn discover() -> Result<Self, StoreError> {
        Ok(Self::new(
            StoragePaths::discover()?,
            UserScopeId::current()?,
        ))
    }

    fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self::with_file_ops(paths, user_scope, Arc::new(RealAtomicFileOps))
    }

    fn with_file_ops(
        paths: StoragePaths,
        user_scope: UserScopeId,
        file_ops: Arc<dyn AtomicFileOps>,
    ) -> Self {
        Self {
            paths,
            user_scope,
            file_ops,
            lock_timeout: GEOMETRY_LOCK_TIMEOUT,
        }
    }

    fn load(&self) -> Result<LoadState<GeometryFile>, StoreError> {
        let _process_guard = GEOMETRY_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    fn mutate<R>(&self, mutation: impl FnOnce(&mut GeometryFile) -> R) -> Result<R, StoreError> {
        let _process_guard = GEOMETRY_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;
        ensure_geometry_writable(self.load_or_migrate_unlocked()?)?;
        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::WindowGeometry,
            &self.user_scope,
            self.paths.window_geometry(),
            self.lock_timeout,
        )?;
        let mut file = self
            .load_v2_unlocked()
            .into_writable_for(StoreKind::WindowGeometry)?;
        let result = mutation(&mut file);
        file.version = GEOMETRY_VERSION;
        validate_geometry_file(&file)?;
        self.write_v2_unlocked(&file)?;

        match self.load_v2_unlocked() {
            LoadState::Loaded(read_back) if read_back == file => Ok(result),
            LoadState::Loaded(_) => Err(geometry_invalid_payload()),
            state => Err(geometry_load_state_error(state)),
        }
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<GeometryFile>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.window_geometry().exists() {
            return Ok(self.load_v2_unlocked());
        }

        let source = self.paths.legacy_window_geometry();
        if !source.exists() {
            return Ok(LoadState::Missing);
        }
        let raw = match fs::read(&source) {
            Ok(raw) => raw,
            Err(source) => {
                return Ok(LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                });
            }
        };
        let (file, source_version) = match decode_legacy_geometry_bytes(&raw) {
            Ok(file) => file,
            Err(error) => return Ok(geometry_load_state_from_error(error)),
        };
        let destination_bytes = serde_json::to_vec(&file).map_err(|_| StoreError::Io {
            store: StoreKind::WindowGeometry,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        let request = MigrationRequest {
            store: StoreKind::WindowGeometry,
            source_path: source,
            source_version,
            destination_path: self.paths.window_geometry().to_path_buf(),
            destination_bytes,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_v2_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_v2_unlocked())
    }

    fn load_v2_unlocked(&self) -> LoadState<GeometryFile> {
        let raw = match fs::read(self.paths.window_geometry()) {
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
            Ok(file) => LoadState::Loaded(file),
            Err(error) => geometry_load_state_from_error(error),
        }
    }

    fn decode_v2_bytes(&self, raw: &[u8]) -> Result<GeometryFile, StoreError> {
        decode_geometry_bytes(raw)
    }

    fn write_v2_unlocked(&self, file: &GeometryFile) -> Result<(), StoreError> {
        validate_geometry_file(file)?;
        write_verified_for(
            StoreKind::WindowGeometry,
            self.paths.window_geometry(),
            file,
            Protection::Plaintext,
            self.file_ops.as_ref(),
        )?;
        restrict_storage_file(self.paths.window_geometry()).map_err(|source| StoreError::Io {
            store: StoreKind::WindowGeometry,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn geometry_invalid_payload() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::WindowGeometry,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

fn decode_legacy_geometry_bytes(raw: &[u8]) -> Result<(GeometryFile, u32), StoreError> {
    let mut file: GeometryFile =
        serde_json::from_slice(raw).map_err(|_| geometry_invalid_payload())?;
    let source_version = file.version;
    normalize_geometry_file(&mut file)?;
    Ok((file, source_version))
}

fn decode_geometry_bytes(raw: &[u8]) -> Result<GeometryFile, StoreError> {
    let mut file: GeometryFile =
        serde_json::from_slice(raw).map_err(|_| geometry_invalid_payload())?;
    normalize_geometry_file(&mut file)?;
    Ok(file)
}

fn normalize_geometry_file(file: &mut GeometryFile) -> Result<(), StoreError> {
    if file.version > GEOMETRY_VERSION {
        return Err(StoreError::UnsupportedVersion {
            store: StoreKind::WindowGeometry,
            found: file.version,
            supported: GEOMETRY_VERSION,
        });
    }
    migrate(file);
    validate_geometry_file(file)
}

fn validate_geometry_file(file: &GeometryFile) -> Result<(), StoreError> {
    if file.version != GEOMETRY_VERSION
        || file.entries.len() > MAX_GEOMETRY_ENTRIES
        || file.size_entries.len() > MAX_GEOMETRY_ENTRIES
        || file.entries.iter().any(|(key, geometry)| {
            !valid_geometry_key(key)
                || geometry.width.is_some_and(|width| !valid_dimension(width))
                || geometry
                    .height
                    .is_some_and(|height| !valid_dimension(height))
        })
        || file.size_entries.iter().any(|(key, size)| {
            !valid_geometry_key(key)
                || !valid_dimension(size.width)
                || !valid_dimension(size.height)
        })
    {
        return Err(geometry_invalid_payload());
    }
    let value = serde_json::to_value(file).map_err(|_| geometry_invalid_payload())?;
    let round_trip: GeometryFile =
        serde_json::from_value(value).map_err(|_| geometry_invalid_payload())?;
    if round_trip != *file {
        return Err(geometry_invalid_payload());
    }
    Ok(())
}

fn valid_geometry_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_GEOMETRY_KEY_LENGTH && !key.chars().any(char::is_control)
}

fn valid_dimension(dimension: u32) -> bool {
    (1..=MAX_LOGICAL_DIMENSION).contains(&dimension)
}

fn ensure_geometry_writable(state: LoadState<GeometryFile>) -> Result<(), StoreError> {
    state
        .into_writable_for(StoreKind::WindowGeometry)
        .map(|_| ())
}

fn geometry_load_state_from_error(error: StoreError) -> LoadState<GeometryFile> {
    match error {
        StoreError::CorruptEnvelope { reason, .. } => LoadState::CorruptEnvelope { reason },
        StoreError::DecryptFailed { reason, .. } => LoadState::DecryptFailed { reason },
        StoreError::InvalidPayload { reason, .. } => LoadState::InvalidPayload { reason },
        StoreError::UnsupportedVersion {
            found, supported, ..
        } => LoadState::UnsupportedVersion { found, supported },
        StoreError::LockTimeout { reason, .. } | StoreError::Locked { reason, .. } => {
            LoadState::Locked { reason }
        }
        StoreError::Io {
            operation, reason, ..
        } => LoadState::IoError { operation, reason },
        StoreError::MigrationRequired { .. } | StoreError::MutationRejected { .. } => {
            LoadState::InvalidPayload {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            }
        }
    }
}

fn geometry_load_state_error(state: LoadState<GeometryFile>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::WindowGeometry,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => geometry_invalid_payload(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::WindowGeometry,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::WindowGeometry,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::WindowGeometry,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::WindowGeometry,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::WindowGeometry,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::WindowGeometry,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::WindowGeometry,
            operation,
            reason,
        },
    }
}

/// Surface modes eligible for geometry persistence.
///
/// - `Hidden`: never remembered.
/// - `TrayPanel`: remembered for SIZE — the "Pop Out Dashboard" flyout is always
///   re-anchored above the tray icon (its position comes from
///   `default_surface_position`, which ignores the stored x/y), but its
///   width/height persist so a user resize sticks across opens.
/// - `PopOut` / `Settings`: user-movable, position + size remembered.
pub fn should_remember(mode: SurfaceMode) -> bool {
    matches!(
        mode,
        SurfaceMode::TrayPanel | SurfaceMode::PopOut | SurfaceMode::Settings
    )
}

fn load_file() -> GeometryFile {
    match GeometryRepository::discover().and_then(|repository| repository.load()) {
        Ok(LoadState::Loaded(file)) => file,
        Ok(LoadState::Missing) => GeometryFile::default(),
        Ok(state) => {
            tracing::warn!(
                target: "codexbar::geometry",
                store = %StoreKind::WindowGeometry,
                state = ?state.kind(),
                "window geometry is unavailable; using defaults without overwriting it"
            );
            GeometryFile::default()
        }
        Err(error) => {
            tracing::warn!(
                target: "codexbar::geometry",
                store = %StoreKind::WindowGeometry,
                %error,
                "window geometry read failed; using defaults without overwriting it"
            );
            GeometryFile::default()
        }
    }
}

/// Bring an on-disk file up to `GEOMETRY_VERSION`. Legacy (versionless) files
/// stored window SIZE in physical pixels, but the restore path now treats
/// stored size as logical; drop those sizes so windows reopen at their default
/// (logical) size and re-persist correct dimensions on the first user move,
/// instead of opening ~scale_factor too large on HiDPI displays.
fn migrate(file: &mut GeometryFile) {
    if file.version < GEOMETRY_VERSION {
        for geometry in file.entries.values_mut() {
            geometry.width = None;
            geometry.height = None;
        }
        file.version = GEOMETRY_VERSION;
    }
}

/// Look up remembered geometry for a surface mode. Returns `None` when the
/// mode is not eligible or no entry has been persisted yet.
pub fn load(mode: SurfaceMode) -> Option<StoredGeometry> {
    if !should_remember(mode) {
        return None;
    }
    load_entry(mode.as_str())
}

/// Persist geometry for an eligible surface mode. No-op for modes where
/// `should_remember` returns `false`.
pub fn save(mode: SurfaceMode, geometry: StoredGeometry) {
    if !should_remember(mode) {
        return;
    }
    save_entry(mode.as_str(), geometry);
}

/// Queue high-frequency geometry updates without doing disk I/O in a native
/// window event callback. The writer coalesces each key to its latest value.
pub fn queue_save(mode: SurfaceMode, geometry: StoredGeometry) {
    if !should_remember(mode) {
        return;
    }
    queue_save_entry(mode.as_str(), geometry);
}

/// Look up remembered geometry for an arbitrary key (e.g. an auxiliary
/// window label like `floatbar`).
pub fn load_entry(key: &str) -> Option<StoredGeometry> {
    load_file().entries.get(key).copied()
}

/// Persist geometry under an arbitrary key.
pub fn save_entry(key: &str, geometry: StoredGeometry) {
    // A direct caller establishes an immediate durability boundary, so flush
    // an older queued position first and never let it overwrite this value.
    flush_pending();
    if let Err(error) = persist_geometry_entry(key, geometry) {
        tracing::warn!(
            target: "codexbar::geometry",
            store = %StoreKind::WindowGeometry,
            %error,
            "failed to persist window geometry"
        );
    }
}

/// Queue geometry under an arbitrary key. This is the event-loop-safe form of
/// [`save_entry`] for high-frequency move/resize notifications.
pub fn queue_save_entry(key: &str, geometry: StoredGeometry) {
    GEOMETRY_SAVE_QUEUE.enqueue(key.to_string(), geometry);
}

/// Flush queued geometry before a controlled close or normal application exit.
/// A bounded wait is intentional: a failed storage lock must not hang shutdown.
pub fn flush_pending() {
    if !GEOMETRY_SAVE_QUEUE.flush() {
        tracing::warn!(
            target: "codexbar::geometry",
            store = %StoreKind::WindowGeometry,
            "timed out while flushing queued window geometry"
        );
    }
}

fn persist_geometry_entry(key: &str, geometry: StoredGeometry) -> Result<(), StoreError> {
    let key = key.to_string();
    GeometryRepository::discover().and_then(|repository| {
        repository.mutate(move |file| {
            file.entries.insert(key, geometry);
        })
    })
}

/// Legacy key the flyout size was stored under before it became a dedicated
/// window: the old `SurfaceMode::TrayPanel` shared-window geometry entry.
const LEGACY_FLYOUT_SIZE_KEY: &str = "trayPanel";

/// Resolve a size-only entry from an in-memory [`GeometryFile`]: prefer the
/// new `size_entries` map, falling back to migrating a pre-existing
/// `"trayPanel"` [`StoredGeometry`] width/height (from before the flyout was
/// split into its own window) so upgrading users keep their remembered size
/// instead of it silently resetting to the default. Pure/side-effect-free so
/// it can be unit-tested without touching disk; [`load_size`] is the
/// disk-backed wrapper that also persists the migrated value.
fn resolve_size(file: &GeometryFile, key: &str) -> Option<StoredSize> {
    if let Some(size) = file.size_entries.get(key).copied() {
        return Some(size);
    }
    if key != LEGACY_FLYOUT_SIZE_KEY {
        let legacy = file.entries.get(LEGACY_FLYOUT_SIZE_KEY)?;
        let (width, height) = (legacy.width?, legacy.height?);
        return Some(StoredSize { width, height });
    }
    None
}

/// Look up a remembered size-only entry (e.g. the flyout window's
/// user-chosen width/height). See [`resolve_size`] for the migration
/// fallback; a migrated value is re-persisted under the new key so this
/// lookup path is only taken once (the legacy entry is left in place —
/// harmless, since nothing reads `entries["trayPanel"]` as a position
/// anymore).
pub fn load_size(key: &str) -> Option<StoredSize> {
    let file = load_file();
    let size = resolve_size(&file, key)?;
    if !file.size_entries.contains_key(key) {
        save_size(key, size);
    }
    Some(size)
}

/// Persist a size-only entry under an arbitrary key.
pub fn save_size(key: &str, size: StoredSize) {
    let key = key.to_string();
    if let Err(error) = GeometryRepository::discover().and_then(|repository| {
        repository.mutate(move |file| {
            file.size_entries.insert(key, size);
        })
    }) {
        tracing::warn!(
            target: "codexbar::geometry",
            store = %StoreKind::WindowGeometry,
            %error,
            "failed to persist window size"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar::storage::{
        AtomicFileOps, LoadState, RealAtomicFileOps, StoragePaths, UserScopeId,
    };
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn repository_fixture() -> (PathBuf, StoragePaths, GeometryRepository) {
        let root = std::env::temp_dir().join(format!("codexbar-geometry-{}", uuid::Uuid::new_v4()));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = GeometryRepository::new(
            paths.clone(),
            UserScopeId::from_stable_identifier(
                format!("geometry-test-{}", uuid::Uuid::new_v4()).as_bytes(),
            ),
        );
        (root, paths, repository)
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FailingAtomicOperation {
        ReplaceTarget,
        ReadBackTarget,
    }

    struct FailingAtomicOps {
        target: PathBuf,
        operation: FailingAtomicOperation,
    }

    impl AtomicFileOps for FailingAtomicOps {
        fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
            RealAtomicFileOps.write_new(path, bytes)
        }

        fn sync_file(&self, path: &Path) -> io::Result<()> {
            RealAtomicFileOps.sync_file(path)
        }

        fn copy_new(&self, source: &Path, destination: &Path) -> io::Result<()> {
            RealAtomicFileOps.copy_new(source, destination)
        }

        fn replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
            if self.operation == FailingAtomicOperation::ReplaceTarget && destination == self.target
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected target replacement failure",
                ));
            }
            RealAtomicFileOps.replace(source, destination)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            if self.operation == FailingAtomicOperation::ReadBackTarget && path == self.target {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected target read-back failure",
                ));
            }
            RealAtomicFileOps.read(path)
        }

        fn remove_if_exists(&self, path: &Path) -> io::Result<()> {
            RealAtomicFileOps.remove_if_exists(path)
        }

        fn sync_parent(&self, path: &Path) -> io::Result<()> {
            RealAtomicFileOps.sync_parent(path)
        }
    }

    #[test]
    fn legacy_geometry_migrates_once_and_later_legacy_writes_are_ignored() {
        let (root, paths, repository) = repository_fixture();
        std::fs::create_dir_all(paths.legacy_window_geometry().parent().unwrap()).unwrap();
        std::fs::write(
            paths.legacy_window_geometry(),
            r#"{"version":1,"entries":{"settings":{"x":100,"y":200,"width":520,"height":600}}}"#,
        )
        .unwrap();

        let LoadState::Loaded(migrated) = repository.load().unwrap() else {
            panic!("legacy geometry did not migrate")
        };
        assert_eq!(migrated.entries["settings"].x, 100);
        assert!(paths.window_geometry().is_file());
        assert!(paths.legacy_window_geometry().is_file());

        std::fs::write(
            paths.legacy_window_geometry(),
            r#"{"version":1,"entries":{"settings":{"x":999,"y":999}}}"#,
        )
        .unwrap();
        let LoadState::Loaded(reloaded) = repository.load().unwrap() else {
            panic!("v2 geometry did not load")
        };
        assert_eq!(reloaded.entries["settings"].x, 100);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn geometry_mutations_merge_entries_under_the_v2_store_lock() {
        let (root, _paths, repository) = repository_fixture();
        let first = repository.clone();
        let second = repository.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);

        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first
                .mutate(|file| {
                    file.entries.insert(
                        "settings".into(),
                        StoredGeometry {
                            x: 10,
                            y: 20,
                            width: Some(520),
                            height: Some(600),
                        },
                    );
                })
                .unwrap();
        });
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            second
                .mutate(|file| {
                    file.entries.insert(
                        "floatbar".into(),
                        StoredGeometry {
                            x: 30,
                            y: 40,
                            width: None,
                            height: None,
                        },
                    );
                })
                .unwrap();
        });
        first_thread.join().unwrap();
        second_thread.join().unwrap();

        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("geometry store not loaded")
        };
        assert_eq!(saved.entries.len(), 2);
        assert_eq!(saved.entries["settings"].x, 10);
        assert_eq!(saved.entries["floatbar"].x, 30);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn geometry_replace_failure_preserves_the_previous_verified_file() {
        let (root, paths, repository) = repository_fixture();
        repository
            .mutate(|file| {
                file.entries.insert(
                    "settings".into(),
                    StoredGeometry {
                        x: 10,
                        y: 20,
                        width: None,
                        height: None,
                    },
                );
            })
            .unwrap();
        let before = std::fs::read(paths.window_geometry()).unwrap();
        let failing = GeometryRepository::with_file_ops(
            paths.clone(),
            repository.user_scope.clone(),
            Arc::new(FailingAtomicOps {
                target: paths.window_geometry().to_path_buf(),
                operation: FailingAtomicOperation::ReplaceTarget,
            }),
        );

        assert!(
            failing
                .mutate(|file| {
                    file.entries.insert(
                        "floatbar".into(),
                        StoredGeometry {
                            x: 30,
                            y: 40,
                            width: None,
                            height: None,
                        },
                    );
                })
                .is_err()
        );
        assert_eq!(std::fs::read(paths.window_geometry()).unwrap(), before);
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("geometry store not loaded")
        };
        assert!(!saved.entries.contains_key("floatbar"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn geometry_read_back_failure_leaves_a_parseable_target_for_idempotent_retry() {
        let (root, paths, repository) = repository_fixture();
        repository
            .mutate(|file| {
                file.entries.insert(
                    "settings".into(),
                    StoredGeometry {
                        x: 10,
                        y: 20,
                        width: None,
                        height: None,
                    },
                );
            })
            .unwrap();
        let failing = GeometryRepository::with_file_ops(
            paths.clone(),
            repository.user_scope.clone(),
            Arc::new(FailingAtomicOps {
                target: paths.window_geometry().to_path_buf(),
                operation: FailingAtomicOperation::ReadBackTarget,
            }),
        );

        assert!(
            failing
                .mutate(|file| {
                    file.entries.insert(
                        "floatbar".into(),
                        StoredGeometry {
                            x: 30,
                            y: 40,
                            width: None,
                            height: None,
                        },
                    );
                })
                .is_err()
        );
        let LoadState::Loaded(after_read_back_failure) = repository.load().unwrap() else {
            panic!("target became unreadable after failed read-back")
        };
        assert!(after_read_back_failure.entries.contains_key("floatbar"));

        repository
            .mutate(|file| {
                file.entries.insert(
                    "floatbar".into(),
                    StoredGeometry {
                        x: 30,
                        y: 40,
                        width: None,
                        height: None,
                    },
                );
            })
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn future_v2_geometry_is_never_overwritten_by_an_older_program() {
        let (root, paths, repository) = repository_fixture();
        std::fs::create_dir_all(paths.window_geometry().parent().unwrap()).unwrap();
        let future_bytes = br#"{"version":99,"entries":{"settings":{"x":100,"y":200}}}"#;
        std::fs::write(paths.window_geometry(), future_bytes).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::UnsupportedVersion {
                found: 99,
                supported: GEOMETRY_VERSION
            }
        ));
        assert!(
            repository
                .mutate(|file| {
                    file.entries.insert(
                        "settings".into(),
                        StoredGeometry {
                            x: 1,
                            y: 2,
                            width: None,
                            height: None,
                        },
                    );
                })
                .is_err()
        );
        assert_eq!(
            std::fs::read(paths.window_geometry()).unwrap(),
            future_bytes
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pop_out_and_settings_are_remembered() {
        assert!(should_remember(SurfaceMode::PopOut));
        assert!(should_remember(SurfaceMode::Settings));
    }

    #[test]
    fn tray_panel_is_remembered_for_size() {
        // The flyout persists its size (position is always re-anchored).
        assert!(should_remember(SurfaceMode::TrayPanel));
    }

    #[test]
    fn hidden_is_not_remembered() {
        assert!(!should_remember(SurfaceMode::Hidden));
    }

    #[test]
    fn non_remembered_mode_save_is_noop() {
        // Call should not panic or error for ineligible modes.
        save(
            SurfaceMode::Hidden,
            StoredGeometry {
                x: 1,
                y: 2,
                width: Some(420),
                height: Some(560),
            },
        );
        assert!(load(SurfaceMode::Hidden).is_none());
    }

    #[test]
    fn geometry_file_round_trip() {
        let mut f = GeometryFile::default();
        f.entries.insert(
            "settings".into(),
            StoredGeometry {
                x: 100,
                y: 200,
                width: Some(520),
                height: Some(600),
            },
        );
        let json = serde_json::to_string(&f).unwrap();
        let parsed: GeometryFile = serde_json::from_str(&json).unwrap();
        let entry = parsed.entries.get("settings").unwrap();
        assert_eq!(entry.x, 100);
        assert_eq!(entry.y, 200);
        assert_eq!(entry.width, Some(520));
        assert_eq!(entry.height, Some(600));
    }

    #[test]
    fn legacy_versionless_file_drops_physical_sizes_on_load() {
        // Pre-v1 files stored SIZE in physical pixels and had no `version`.
        // Migration must drop those sizes (keeping position) so a HiDPI upgrade
        // doesn't reopen the window scale_factor-too-large.
        let json = r#"{"entries":{"settings":{"x":10,"y":20,"width":744,"height":1116}}}"#;
        let mut file: GeometryFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.version, 0);
        migrate(&mut file);
        assert_eq!(file.version, GEOMETRY_VERSION);
        let entry = file.entries.get("settings").unwrap();
        assert_eq!(entry.x, 10);
        assert_eq!(entry.y, 20);
        assert_eq!(entry.width, None);
        assert_eq!(entry.height, None);
    }

    #[test]
    fn current_version_file_keeps_sizes() {
        let json =
            r#"{"version":1,"entries":{"settings":{"x":10,"y":20,"width":520,"height":600}}}"#;
        let mut file: GeometryFile = serde_json::from_str(json).unwrap();
        migrate(&mut file);
        let entry = file.entries.get("settings").unwrap();
        assert_eq!(entry.width, Some(520));
        assert_eq!(entry.height, Some(600));
    }

    #[test]
    fn geometry_file_parses_without_size() {
        let json = r#"{"entries":{"settings":{"x":10,"y":20}}}"#;
        let parsed: GeometryFile = serde_json::from_str(json).unwrap();
        let entry = parsed.entries.get("settings").unwrap();
        assert_eq!(entry.x, 10);
        assert_eq!(entry.y, 20);
        assert_eq!(entry.width, None);
        assert_eq!(entry.height, None);
    }

    #[test]
    fn stored_size_round_trips_without_position_fields() {
        let mut f = GeometryFile::default();
        f.size_entries.insert(
            "flyout".into(),
            StoredSize {
                width: 400,
                height: 820,
            },
        );
        let json = serde_json::to_string(&f).unwrap();
        // The size-only entry must never carry x/y — that's the whole point
        // of keeping it out of `entries: BTreeMap<String, StoredGeometry>`.
        assert!(!json.contains("\"x\""));
        assert!(!json.contains("\"y\""));
        let parsed: GeometryFile = serde_json::from_str(&json).unwrap();
        let entry = parsed.size_entries.get("flyout").unwrap();
        assert_eq!(entry.width, 400);
        assert_eq!(entry.height, 820);
    }

    #[test]
    fn resolve_size_prefers_new_key_over_legacy() {
        let mut file = GeometryFile::default();
        file.size_entries.insert(
            "flyout".into(),
            StoredSize {
                width: 500,
                height: 900,
            },
        );
        file.entries.insert(
            LEGACY_FLYOUT_SIZE_KEY.into(),
            StoredGeometry {
                x: 0,
                y: 0,
                width: Some(640),
                height: Some(720),
            },
        );

        let resolved = resolve_size(&file, "flyout").expect("size present");
        assert_eq!(resolved.width, 500);
        assert_eq!(resolved.height, 900);
    }

    #[test]
    fn resolve_size_migrates_legacy_tray_panel_geometry_when_no_new_entry() {
        // Simulates an upgrading user: pre-refactor size lived under the
        // `SurfaceMode::TrayPanel` shared-window geometry key.
        let mut file = GeometryFile::default();
        file.entries.insert(
            LEGACY_FLYOUT_SIZE_KEY.into(),
            StoredGeometry {
                x: 0,
                y: 0,
                width: Some(640),
                height: Some(720),
            },
        );

        let resolved = resolve_size(&file, "flyout").expect("legacy size migrates");
        assert_eq!(resolved.width, 640);
        assert_eq!(resolved.height, 720);
    }

    #[test]
    fn resolve_size_ignores_legacy_entry_missing_width_or_height() {
        let mut file = GeometryFile::default();
        file.entries.insert(
            LEGACY_FLYOUT_SIZE_KEY.into(),
            StoredGeometry {
                x: 0,
                y: 0,
                width: Some(640),
                height: None,
            },
        );

        assert!(resolve_size(&file, "flyout").is_none());
    }

    #[test]
    fn resolve_size_returns_none_when_nothing_stored() {
        let file = GeometryFile::default();
        assert!(resolve_size(&file, "flyout").is_none());
    }

    #[test]
    fn resolve_size_for_legacy_key_itself_does_not_self_migrate() {
        // Looking up the legacy key directly should only consult
        // `size_entries` (the `key != LEGACY_FLYOUT_SIZE_KEY` guard) — no
        // infinite fallback to itself.
        let file = GeometryFile::default();
        assert!(resolve_size(&file, LEGACY_FLYOUT_SIZE_KEY).is_none());
    }

    #[test]
    fn geometry_write_coalescer_keeps_only_the_final_distinct_geometry() {
        use std::time::Instant;

        let start = Instant::now();
        let first = StoredGeometry {
            x: 10,
            y: 20,
            width: Some(500),
            height: Some(400),
        };
        let final_geometry = StoredGeometry {
            x: 110,
            y: 220,
            width: Some(600),
            height: Some(450),
        };
        let mut coalescer = GeometryWriteCoalescer::default();

        coalescer.enqueue("settings".into(), first, start);
        assert!(
            coalescer
                .take_ready(start + GEOMETRY_WRITE_DEBOUNCE - Duration::from_millis(1))
                .is_empty()
        );

        let updated_at = start + Duration::from_millis(50);
        coalescer.enqueue("settings".into(), final_geometry, updated_at);
        assert!(
            coalescer
                .take_ready(updated_at + GEOMETRY_WRITE_DEBOUNCE - Duration::from_millis(1))
                .is_empty()
        );
        assert_eq!(
            coalescer.take_ready(updated_at + GEOMETRY_WRITE_DEBOUNCE),
            vec![("settings".to_string(), final_geometry)]
        );

        coalescer.acknowledge("settings", final_geometry);
        coalescer.enqueue(
            "settings".into(),
            final_geometry,
            updated_at + GEOMETRY_WRITE_DEBOUNCE + Duration::from_millis(1),
        );
        assert!(coalescer.take_all().is_empty());
    }
}
