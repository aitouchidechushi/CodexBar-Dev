use std::collections::BTreeMap;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use codexbar::secure_file::{restrict_storage_directory, restrict_storage_file};
use sha2::{Digest, Sha256};
use uuid::Uuid;

type DirectoryManifest = BTreeMap<PathBuf, Option<[u8; 32]>>;

// The caller holds the session-root migration lock until account metadata commits.
// Original profiles are retained. A completed directory can be reused after a
// crash before the metadata commit only if all its contents still match.
pub(super) fn copy_session_verified(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(source) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(metadata) => validate_directory(&metadata)?,
    }
    let expected = directory_manifest(source)?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            validate_directory(&metadata)?;
            return if directory_manifest(destination)? == expected {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "session migration conflict",
                ))
            };
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let parent = destination.parent().ok_or_else(invalid_directory)?;
    fs::create_dir_all(parent)?;
    validate_directory(&fs::symlink_metadata(parent)?)?;
    restrict_storage_directory(parent)?;
    let staging = parent.join(format!(".session-migration-{}", Uuid::new_v4()));
    fs::create_dir(&staging)?;
    restrict_storage_directory(&staging)?;
    for (relative, hash) in &expected {
        let target = staging.join(relative);
        if hash.is_none() {
            fs::create_dir(&target)?;
            restrict_storage_directory(&target)?;
        } else {
            let original = source.join(relative);
            let metadata = fs::symlink_metadata(&original)?;
            if is_link(&metadata) || !metadata.is_file() {
                return Err(invalid_directory());
            }
            fs::copy(&original, &target)?;
            restrict_storage_file(&target)?;
            OpenOptions::new().write(true).open(&target)?.sync_all()?;
        }
    }
    if directory_manifest(&staging)? != expected || directory_manifest(source)? != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "session changed during migration",
        ));
    }
    // A failure retains the private staging directory for recovery; never remove
    // the source or replace a different destination to force migration forward.
    fs::rename(&staging, destination)
}

fn directory_manifest(root: &Path) -> io::Result<DirectoryManifest> {
    validate_directory(&fs::symlink_metadata(root)?)?;
    let mut manifest = BTreeMap::new();
    let mut directories = vec![PathBuf::new()];
    while let Some(relative) = directories.pop() {
        for entry in fs::read_dir(root.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())?;
            if is_link(&metadata) {
                return Err(invalid_directory());
            }
            if metadata.is_dir() {
                manifest.insert(path.clone(), None);
                directories.push(path);
            } else if metadata.is_file() {
                manifest.insert(path, Some(file_hash(&entry.path())?));
            } else {
                return Err(invalid_directory());
            }
        }
    }
    Ok(manifest)
}

fn file_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(hash.finalize().into());
        }
        hash.update(&buffer[..count]);
    }
}

fn validate_directory(metadata: &Metadata) -> io::Result<()> {
    if metadata.is_dir() && !is_link(metadata) {
        Ok(())
    } else {
        Err(invalid_directory())
    }
}

fn invalid_directory() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid session migration directory",
    )
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_accepts_a_complete_copy_without_overwriting_it() {
        let root = std::env::temp_dir().join(format!("codexbar-session-copy-{}", Uuid::new_v4()));
        let source = root.join("legacy");
        let destination = root.join("v2").join("session");
        fs::create_dir_all(source.join("empty")).unwrap();
        fs::write(source.join("data"), b"session-fixture").unwrap();
        copy_session_verified(&source, &destination).unwrap();
        copy_session_verified(&source, &destination).unwrap();
        assert!(destination.join("empty").is_dir());
        assert_eq!(
            fs::read(destination.join("data")).unwrap(),
            b"session-fixture"
        );
        assert_eq!(fs::read(source.join("data")).unwrap(), b"session-fixture");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn conflicting_destination_blocks_migration_and_preserves_both_copies() {
        let root = std::env::temp_dir().join(format!("codexbar-session-copy-{}", Uuid::new_v4()));
        let source = root.join("legacy");
        let destination = root.join("v2");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("data"), b"legacy-fixture").unwrap();
        fs::write(destination.join("data"), b"newer-fixture").unwrap();
        assert_eq!(
            copy_session_verified(&source, &destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(source.join("data")).unwrap(), b"legacy-fixture");
        assert_eq!(
            fs::read(destination.join("data")).unwrap(),
            b"newer-fixture"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
