use anyhow::{bail, Context, Result};
use same_file::Handle;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek},
    path::Path,
};

pub(crate) const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

pub(crate) enum SourceRead {
    Snapshot(String),
    TooLarge,
}

pub(crate) fn read_source_snapshot(path: &Path) -> Result<SourceRead> {
    const MAX_SNAPSHOT_ATTEMPTS: usize = 3;
    let mut last_error = None;
    for _ in 0..MAX_SNAPSHOT_ATTEMPTS {
        match read_source_snapshot_once(path) {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("snapshot attempts are nonzero"))
}

fn read_source_snapshot_once(path: &Path) -> Result<SourceRead> {
    let mut file =
        open_source(path).with_context(|| format!("open source snapshot {}", path.display()))?;
    let opened_identity = Handle::from_file(
        file.try_clone()
            .with_context(|| format!("clone source snapshot {}", path.display()))?,
    )
    .with_context(|| format!("identify source snapshot {}", path.display()))?;
    let before = file
        .metadata()
        .with_context(|| format!("stat source snapshot {}", path.display()))?;
    let before_modified = before
        .modified()
        .with_context(|| format!("timestamp source snapshot {}", path.display()))?;
    if !before.is_file() {
        bail!("source snapshot is not a regular file: {}", path.display());
    }
    if before.len() > MAX_SOURCE_BYTES {
        let current = reopened_metadata(path, &opened_identity)?;
        let current_modified = current
            .modified()
            .with_context(|| format!("retimestamp reopened source {}", path.display()))?;
        if current.len() != before.len()
            || current_modified != before_modified
            || current.len() <= MAX_SOURCE_BYTES
        {
            bail!(
                "source changed while classifying its size: {}",
                path.display()
            );
        }
        return Ok(SourceRead::TooLarge);
    }

    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file)
        .take(MAX_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read source snapshot {}", path.display()))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        let after = file
            .metadata()
            .with_context(|| format!("restat oversized source snapshot {}", path.display()))?;
        let after_modified = after
            .modified()
            .with_context(|| format!("retimestamp oversized source snapshot {}", path.display()))?;
        let current = reopened_metadata(path, &opened_identity)?;
        let current_modified = current
            .modified()
            .with_context(|| format!("retimestamp reopened source {}", path.display()))?;
        if after.len() <= MAX_SOURCE_BYTES
            || current.len() != after.len()
            || current_modified != after_modified
        {
            bail!(
                "source changed while classifying its size: {}",
                path.display()
            );
        }
        return Ok(SourceRead::TooLarge);
    }
    file.rewind()
        .with_context(|| format!("rewind source snapshot {}", path.display()))?;
    let mut verification = Vec::with_capacity(bytes.len());
    (&mut file)
        .take(MAX_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut verification)
        .with_context(|| format!("verify source snapshot {}", path.display()))?;
    if verification != bytes {
        bail!("source changed while taking snapshot: {}", path.display());
    }

    let after = file
        .metadata()
        .with_context(|| format!("restat source snapshot {}", path.display()))?;
    let after_modified = after
        .modified()
        .with_context(|| format!("retimestamp source snapshot {}", path.display()))?;
    let current_metadata = reopened_metadata(path, &opened_identity)?;
    let current_modified = current_metadata
        .modified()
        .with_context(|| format!("retimestamp reopened source {}", path.display()))?;
    if before.len() != current_metadata.len()
        || before.len() != after.len()
        || before_modified != after_modified
        || after_modified != current_modified
        || after.len() != bytes.len() as u64
    {
        bail!("source changed while taking snapshot: {}", path.display());
    }
    Ok(SourceRead::Snapshot(sanitize_utf8(bytes)))
}

fn reopened_metadata(path: &Path, opened_identity: &Handle) -> Result<std::fs::Metadata> {
    let current_file =
        open_source(path).with_context(|| format!("reopen source snapshot {}", path.display()))?;
    let current_metadata = current_file
        .metadata()
        .with_context(|| format!("restat reopened source snapshot {}", path.display()))?;
    if !current_metadata.is_file() {
        bail!("reopened source is not a regular file: {}", path.display());
    }
    let current_identity = Handle::from_file(current_file)
        .with_context(|| format!("reidentify source snapshot {}", path.display()))?;
    if opened_identity != &current_identity {
        bail!(
            "source identity changed while taking snapshot: {}",
            path.display()
        );
    }
    Ok(current_metadata)
}

#[cfg(unix)]
fn open_source(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(windows)]
fn open_source(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_source(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn sanitize_utf8(bytes: Vec<u8>) -> String {
    let bytes = match String::from_utf8(bytes) {
        Ok(source) => return source,
        Err(error) => error.into_bytes(),
    };
    let mut output = Vec::with_capacity(bytes.len());
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(_) => {
                output.extend_from_slice(remaining);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.extend_from_slice(&remaining[..valid]);
                remaining = &remaining[valid..];
                let invalid = error.error_len().unwrap_or(remaining.len());
                for byte in &remaining[..invalid] {
                    output.push(if matches!(*byte, b'\n' | b'\r') {
                        *byte
                    } else {
                        b' '
                    });
                }
                remaining = &remaining[invalid..];
            }
        }
    }
    String::from_utf8(output).expect("invalid UTF-8 bytes are replaced byte-for-byte")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn invalid_utf8_is_replaced_without_moving_source_offsets() {
        let source = sanitize_utf8(b"fn before() {}\n// bad: \xff\nfn after() {}\n".to_vec());
        assert_eq!(source.len(), 39);
        assert_eq!(source.lines().count(), 3);
        assert_eq!(source.find("fn after"), Some(25));
        assert!(source.contains("// bad:  "));
    }

    #[test]
    fn source_snapshot_enforces_the_limit_during_the_read() {
        let root = tempdir().unwrap();
        let exact = root.path().join("exact.rs");
        fs::write(&exact, vec![b'x'; MAX_SOURCE_BYTES as usize]).unwrap();
        let SourceRead::Snapshot(source) = read_source_snapshot(&exact).unwrap() else {
            panic!("a source exactly at the limit must be accepted");
        };
        assert_eq!(source.len(), MAX_SOURCE_BYTES as usize);

        let oversized = root.path().join("oversized.rs");
        fs::write(&oversized, vec![b'x'; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            read_source_snapshot(&oversized).unwrap(),
            SourceRead::TooLarge
        ));
    }

    #[test]
    fn source_snapshot_rejects_non_regular_inputs() {
        let root = tempdir().unwrap();
        assert!(read_source_snapshot(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn source_snapshot_does_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.rs"), "pub fn secret() {}\n").unwrap();
        let link = root.path().join("swapped.rs");
        symlink(outside.path().join("secret.rs"), &link).unwrap();

        assert!(
            read_source_snapshot(&link).is_err(),
            "a path swap must not make the snapshot reader follow a symbolic link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn source_snapshot_does_not_block_on_fifo_inputs() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        let root = tempdir().unwrap();
        let fifo = root.path().join("pipe.rs");
        let fifo_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_path is a live, NUL-terminated path and mkfifo does not
        // retain the pointer after returning.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        assert!(
            read_source_snapshot(&fifo).is_err(),
            "non-regular inputs must fail without waiting for a writer"
        );
    }
}
