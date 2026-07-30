use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

/// Atomically replaces `path` with `bytes`.
///
/// The temporary file is created in the destination directory so publication
/// cannot cross filesystems. An existing destination is replaced atomically on
/// every supported platform.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let (temporary, mut file) = create_temporary(path)?;
    let write_result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)?;
        sync_parent(parent)
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

/// Publishes a completed temporary file over `path`.
///
/// Both paths must share a parent directory. The temporary file is synced
/// before the atomic replacement and the directory is synced afterwards.
pub(crate) fn publish_temporary(temporary: &Path, path: &Path) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if temporary.parent().unwrap_or_else(|| Path::new(".")) != parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic publication requires a temporary file in the destination directory",
        ));
    }
    let file = fs::File::open(temporary)?;
    file.sync_all()?;
    drop(file);
    replace(temporary, path)?;
    sync_parent(parent)
}

fn create_temporary(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("structurely"));

    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = name.clone();
        temporary_name.push(format!(
            ".structurely-{}-{sequence}.tmp",
            std::process::id()
        ));
        let temporary = parent.join(temporary_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique atomic-write temporary file",
    ))
}

#[cfg(unix)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(not(any(unix, windows)))]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains an embedded NUL",
            ));
        }
        encoded.push(0);
        Ok(encoded)
    }

    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeatedly_replaces_existing_file_without_leaving_temporaries() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("state.json");

        for revision in 0..64 {
            let content = format!("revision-{revision}");
            write_atomic(&destination, content.as_bytes()).unwrap();
            assert_eq!(fs::read_to_string(&destination).unwrap(), content);
        }

        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_publication_preserves_destination_and_cleans_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("occupied");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sentinel"), b"original").unwrap();

        assert!(write_atomic(&destination, b"replacement").is_err());
        assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
