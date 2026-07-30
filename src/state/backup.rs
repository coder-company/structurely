use super::{reject_symlink, state_coordination_lock, validate_schema, StateStore, DATABASE_FILE};
use crate::{atomic_file::publish_temporary, engine::PROJECT_DIR};
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_BACKUP_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const TEMPORARY_ATTEMPTS: usize = 128;
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StateBackupReport {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StateRestoreReport {
    pub source: PathBuf,
    pub path: PathBuf,
    pub bytes: u64,
}

pub(super) fn backup(
    store: &StateStore,
    destination: &Path,
    force: bool,
) -> Result<StateBackupReport> {
    let destination = validated_destination(destination)?;
    reject_symlink(&destination)?;
    reject_existing(&destination, force, "backup destination")?;
    let snapshot_temporary = temporary_path(&destination)?;
    let mut publication_temporary = None;

    let result = (|| {
        let temporary_sql = snapshot_temporary
            .to_str()
            .context("backup destination must be valid UTF-8")?;
        store
            .connection
            .execute("VACUUM main INTO ?1", [temporary_sql])
            .with_context(|| {
                format!(
                    "create consistent durable state snapshot {}",
                    destination.display()
                )
            })?;
        validate_database(&snapshot_temporary)?;

        // SQLite's Windows VFS can retain a non-share-delete handle after a
        // successful VACUUM/validation cycle. Publish an exact, fsynced copy
        // that SQLite has never opened so atomic rename remains portable.
        let publication = temporary_path(&destination)?;
        publication_temporary = Some(publication.clone());
        copy_bounded(&snapshot_temporary, &publication)?;
        let bytes = bounded_size(&publication)?;
        publish_temporary(&publication, &destination)
            .with_context(|| format!("publish state backup {}", destination.display()))?;
        Ok(StateBackupReport {
            path: destination,
            bytes,
        })
    })();

    let _ = fs::remove_file(&snapshot_temporary);
    if let Some(publication) = publication_temporary {
        let _ = fs::remove_file(publication);
    }
    result
}

pub(super) fn restore(root: &Path, source: &Path, force: bool) -> Result<StateRestoreReport> {
    anyhow::ensure!(
        force,
        "restoring durable state replaces the live database; pass --force to continue"
    );
    validate_path_bound(source)?;
    reject_symlink(source)?;
    anyhow::ensure!(
        source.is_file(),
        "state backup is not a file: {}",
        source.display()
    );
    validate_database(source)?;
    let source = canonicalize_portable(source)
        .with_context(|| format!("resolve state backup {}", source.display()))?;
    let bytes = bounded_size(&source)?;

    let root = canonicalize_portable(root)?;
    let directory = root.join(PROJECT_DIR);
    fs::create_dir_all(&directory)
        .with_context(|| format!("create state directory {}", directory.display()))?;
    reject_symlink(&directory)?;
    let _coordination_lock = state_coordination_lock(&directory, true)?;
    let destination = directory.join(DATABASE_FILE);
    reject_symlink(&destination)?;
    if destination.exists() {
        let live = canonicalize_portable(&destination)?;
        anyhow::ensure!(
            live != source,
            "state backup and live database must be different files"
        );
    }

    let temporary = temporary_path(&destination)?;
    let result = (|| {
        copy_bounded(&source, &temporary)?;
        validate_database(&temporary)?;

        // A clean checkpoint ensures no old WAL frames can be replayed over the
        // restored database. This happens before publication, preserving the
        // original database if checkpointing or sidecar cleanup fails.
        if destination.exists() {
            checkpoint_live_database(&destination)?;
        }
        remove_sidecar_if_present(&destination.with_extension("db-wal"))?;
        remove_sidecar_if_present(&destination.with_extension("db-shm"))?;

        publish_temporary(&temporary, &destination)
            .with_context(|| format!("publish restored state {}", destination.display()))?;
        Ok(StateRestoreReport {
            source,
            path: destination,
            bytes,
        })
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn validated_destination(path: &Path) -> Result<PathBuf> {
    validate_path_bound(path)?;
    let file_name = path
        .file_name()
        .context("backup destination must name a file")?;
    anyhow::ensure!(!file_name.is_empty(), "backup destination must name a file");
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = canonicalize_portable(parent)
        .with_context(|| format!("resolve backup directory {}", parent.display()))?;
    reject_symlink(&parent)?;
    Ok(parent.join(file_name))
}

fn canonicalize_portable(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    Ok(strip_windows_verbatim_prefix(canonical))
}

#[cfg(not(windows))]
fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

#[cfg(windows)]
fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const VERBATIM_UNC: &[u16] = &[
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        b'\\' as u16,
    ];
    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let normalized = if let Some(rest) = encoded.strip_prefix(VERBATIM_UNC) {
        [vec![b'\\' as u16, b'\\' as u16], rest.to_vec()].concat()
    } else if let Some(rest) = encoded.strip_prefix(VERBATIM) {
        rest.to_vec()
    } else {
        return path;
    };
    PathBuf::from(OsString::from_wide(&normalized))
}

fn validate_path_bound(path: &Path) -> Result<()> {
    let bytes = path.as_os_str().to_string_lossy().len();
    anyhow::ensure!(bytes > 0, "state backup path must not be empty");
    anyhow::ensure!(
        bytes <= MAX_PATH_BYTES,
        "state backup path exceeds {MAX_PATH_BYTES} bytes"
    );
    Ok(())
}

fn reject_existing(path: &Path, force: bool, label: &str) -> Result<()> {
    if path.exists() {
        anyhow::ensure!(
            force,
            "{label} already exists: {}; pass --force to replace it",
            path.display()
        );
        anyhow::ensure!(path.is_file(), "{label} is not a file: {}", path.display());
    }
    Ok(())
}

fn temporary_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("state database path must be valid UTF-8")?;
    for _ in 0..TEMPORARY_ATTEMPTS {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.structurely-{}-{sequence}.tmp",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not allocate a temporary state database")
}

fn copy_bounded(source: &Path, destination: &Path) -> Result<()> {
    let mut input = fs::File::open(source)
        .with_context(|| format!("open state backup {}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("create restore staging file {}", destination.display()))?;
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut input).take(MAX_BACKUP_BYTES + 1),
        &mut output,
    )?;
    anyhow::ensure!(
        copied <= MAX_BACKUP_BYTES,
        "state backup exceeds the {} byte restore limit",
        MAX_BACKUP_BYTES
    );
    output.flush()?;
    output.sync_all()?;
    drop(output);
    drop(input);
    Ok(())
}

fn bounded_size(path: &Path) -> Result<u64> {
    let bytes = fs::metadata(path)?.len();
    anyhow::ensure!(bytes > 0, "state backup is empty: {}", path.display());
    anyhow::ensure!(
        bytes <= MAX_BACKUP_BYTES,
        "state backup exceeds the {} byte limit",
        MAX_BACKUP_BYTES
    );
    Ok(bytes)
}

fn validate_database(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    // SQLite's NOFOLLOW handling on macOS rejects paths containing the
    // system's `/var` -> `/private/var` alias even when the database itself is
    // a regular file. Resolve parent aliases after explicitly rejecting a
    // symlink at the database path; NOFOLLOW still closes a swap race at the
    // canonical filename.
    let sqlite_path = path
        .canonicalize()
        .with_context(|| format!("resolve state backup {}", path.display()))?;
    let connection = Connection::open_with_flags(
        &sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open state backup {}", path.display()))?;
    validate_schema(&connection)
        .with_context(|| format!("validate state backup schema {}", path.display()))?;
    let integrity: String =
        connection.pragma_query_value(None, "integrity_check", |row| row.get(0))?;
    anyhow::ensure!(
        integrity == "ok",
        "state backup failed SQLite integrity check: {integrity}"
    );
    let has_foreign_key_errors = connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some();
    anyhow::ensure!(
        !has_foreign_key_errors,
        "state backup failed foreign-key validation"
    );
    // Close explicitly before atomic publication. Windows does not allow the
    // validated staging database to be renamed while SQLite still owns a file
    // handle, even though Unix permits it.
    connection
        .close()
        .map_err(|(_, error)| error)
        .context("close validated state backup")?;
    Ok(())
}

fn checkpoint_live_database(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    let sqlite_path = path
        .canonicalize()
        .with_context(|| format!("resolve live durable state {}", path.display()))?;
    let connection = Connection::open_with_flags(
        &sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .with_context(|| format!("open live durable state {}", path.display()))?;
    connection.pragma_update(None, "busy_timeout", 5_000)?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .context("checkpoint live durable state before restore")
}

fn remove_sidecar_if_present(path: &Path) -> Result<()> {
    reject_symlink(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("remove stale SQLite sidecar {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_snapshot_includes_wal_and_replaces_later_state() {
        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let snapshot = backup_directory.path().join("state-backup.db");
        let store = StateStore::open(root.path()).unwrap();
        let preserved = store.create_workspace("Preserved").unwrap();

        let report = store.backup(&snapshot, false).unwrap();
        assert_eq!(report.path, snapshot.canonicalize().unwrap());
        assert!(report.bytes > 0);
        store.create_workspace("Created later").unwrap();
        drop(store);

        let restored = StateStore::restore(root.path(), &snapshot, true).unwrap();
        assert_eq!(restored.bytes, report.bytes);
        let reopened = StateStore::open_read_only(root.path()).unwrap().unwrap();
        assert_eq!(
            reopened.list_workspaces(10).unwrap(),
            vec![preserved],
            "the snapshot should include committed WAL state but exclude later writes"
        );
    }

    #[test]
    fn overwrite_and_restore_require_explicit_force() {
        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let snapshot = backup_directory.path().join("state-backup.db");
        let store = StateStore::open(root.path()).unwrap();
        store.create_workspace("Original").unwrap();
        store.backup(&snapshot, false).unwrap();
        assert_eq!(
            fs::read_dir(backup_directory.path()).unwrap().count(),
            1,
            "backup publication left a staging file"
        );

        let backup_error = store.backup(&snapshot, false).unwrap_err().to_string();
        assert!(backup_error.contains("--force"));
        let restore_error = StateStore::restore(root.path(), &snapshot, false)
            .unwrap_err()
            .to_string();
        assert!(restore_error.contains("--force"));
    }

    #[test]
    fn restore_refuses_to_race_an_open_state_store() {
        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let snapshot = backup_directory.path().join("state-backup.db");
        let store = StateStore::open(root.path()).unwrap();
        store.create_workspace("Active").unwrap();
        store.backup(&snapshot, false).unwrap();

        let error = StateStore::restore(root.path(), &snapshot, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("another Structurely process"));
        assert_eq!(store.list_workspaces(10).unwrap().len(), 1);
    }

    #[test]
    fn corrupt_restore_preserves_live_state_and_leaves_no_staging_file() {
        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let corrupt = backup_directory.path().join("corrupt.db");
        fs::write(&corrupt, b"not a SQLite database").unwrap();
        let store = StateStore::open(root.path()).unwrap();
        let workspace = store.create_workspace("Must survive").unwrap();
        drop(store);

        let error = StateStore::restore(root.path(), &corrupt, true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("state backup") || error.contains("database"),
            "unexpected validation error: {error}"
        );
        let reopened = StateStore::open_read_only(root.path()).unwrap().unwrap();
        assert_eq!(reopened.workspace(&workspace.id).unwrap(), Some(workspace));
        let state_directory = root.path().join(PROJECT_DIR);
        assert!(
            fs::read_dir(state_directory).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp")),
            "failed restore left a staging database"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_sources_and_destinations() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let real = backup_directory.path().join("real.db");
        let link = backup_directory.path().join("linked.db");
        let store = StateStore::open(root.path()).unwrap();
        store.create_workspace("Protected").unwrap();
        store.backup(&real, false).unwrap();
        symlink(&real, &link).unwrap();

        assert!(store
            .backup(&link, true)
            .unwrap_err()
            .to_string()
            .contains("symlink"));
        drop(store);
        assert!(StateStore::restore(root.path(), &link, true)
            .unwrap_err()
            .to_string()
            .contains("symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn validation_allows_parent_aliases_but_not_database_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let backup_directory = tempdir().unwrap();
        let alias = root.path().join("backup-directory");
        let snapshot = backup_directory.path().join("state-backup.db");
        let store = StateStore::open(root.path()).unwrap();
        store.create_workspace("Portable").unwrap();
        store.backup(&snapshot, false).unwrap();
        symlink(backup_directory.path(), &alias).unwrap();

        validate_database(&alias.join("state-backup.db")).unwrap();
        let linked_database = root.path().join("linked.db");
        symlink(&snapshot, &linked_database).unwrap();
        assert!(validate_database(&linked_database)
            .unwrap_err()
            .to_string()
            .contains("symlink"));
    }
}
