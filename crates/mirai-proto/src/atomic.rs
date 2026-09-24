// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Crash-safe replacement of a durable file.
//!
//! `std::fs::write` truncates the destination before the new bytes are durable, so a
//! crash or a full disk leaves an empty or partial file. [`write_atomic`] stages a
//! sibling temporary file, syncs it, and renames it over the target instead.
//!
//! The helper lives here because this is the lowest crate allowed to do I/O.
//! `mirai-core` must stay pure, and both `mirai` and `mirai-engine` already depend on
//! `mirai-proto`. It is not behind `quinn-transport`: `mirai-engine` depends on this
//! crate with `default-features = false` and still has to publish a KataGo config.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes temporary files created in this process. Combined with the pid so two
/// processes, and two threads in one process, never share a name.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A sibling of `target` whose name includes this process's pid and a process-wide
/// counter. Hidden by a leading dot so a crash between create and rename does not look
/// like a second config.
fn temp_path(target: &Path) -> PathBuf {
    let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut name = std::ffi::OsString::from(".");
    name.push(target.file_name().unwrap_or(std::ffi::OsStr::new("mirai")));
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

/// Writes `bytes` to `path` by staging a uniquely named sibling, syncing it, and
/// renaming it over `path`.
///
/// The temporary file is removed if any step fails, so `ENOSPC` or a crash before the
/// rename leaves the previous contents intact. The parent directory is synced after the
/// rename, so a power loss after this returns leaves the new contents. Readers never
/// observe a truncated file.
///
/// A symlink is followed, including a dangling one: the link text is resolved against
/// the link's directory, and the bytes land on that target. An existing file keeps its
/// mode; `std::fs::write` did both, and a `0600` config holds remote tokens.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, None)
}

/// Stages `bytes` beside `target` and returns the temporary path. The caller renames it
/// into place. The certificate installer uses this so a key and a cert can land in a
/// fixed order instead of each rename racing the other file.
///
/// On Unix, `mode` is the permission the temporary file is created with, so a private
/// key is `0600` from the moment it exists rather than after a later `chmod`. Other
/// platforms ignore it.
pub(crate) fn stage(target: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<PathBuf> {
    let tmp = temp_path(target);
    if let Err(error) = write_synced(&tmp, bytes, mode) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(tmp)
}

fn write_synced(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let mut file = create_new(path, mode)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn create_new(path: &Path, mode: Option<u32>) -> io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    opts.open(path)
}

fn persist(tmp: PathBuf, target: &Path) -> io::Result<()> {
    if let Err(error) = std::fs::rename(&tmp, target) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

fn replace(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let target = follow(path)?;
    let staged = stage(&target, bytes, mode)?;
    if mode.is_none()
        && let Err(error) = copy_mode(&target, &staged)
    {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    persist(staged, &target)?;
    sync_parent(&target)
}

/// Follows `path` if it is a symlink. A dangling link is resolved with `read_link`
/// against the link's directory — `canonicalize` would fail and the write would not
/// land where the link points.
fn follow(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&current)?;
                current = if link.is_absolute() {
                    link
                } else {
                    match current.parent() {
                        Some(dir) if !dir.as_os_str().is_empty() => dir.join(link),
                        _ => link,
                    }
                };
            }
            Ok(_) => return Ok(current),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "too many symlinks",
    ))
}

fn copy_mode(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::metadata(from) {
        Ok(meta) => std::fs::set_permissions(to, meta.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Syncs the directory that contains `path`, so a rename or unlink is durable across
/// power loss and not only across a crash of this process.
///
/// Unix only: elsewhere a directory cannot be opened as a `File` to sync, and the
/// rename is left to the filesystem's own ordering.
#[cfg(unix)]
pub(crate) fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mirai-atomic-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temps_in(dir: &Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect()
    }

    /// The replacement must be a new inode, not a truncate of the file a reader might
    /// already have open, and the staging name must not outlive the call.
    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        let dir = scratch("ok");
        let path = dir.join("target.txt");
        std::fs::write(&path, b"old").unwrap();
        #[cfg(unix)]
        let before = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&path).unwrap().ino()
        };

        write_atomic(&path, b"new contents").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new contents");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(
                std::fs::metadata(&path).unwrap().ino(),
                before,
                "must replace the inode, not truncate in place"
            );
        }
        assert!(
            temps_in(&dir).is_empty(),
            "staging file left behind: {:?}",
            temps_in(&dir)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rename onto a directory fails. The previous tree, and any sibling file, must
    /// still be there, and the staging file must have been removed.
    #[test]
    fn a_failed_replace_leaves_the_previous_contents() {
        let dir = scratch("dir-target");
        let target = dir.join("not-a-file");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("inside"), b"keep").unwrap();
        let sibling = dir.join("sibling.txt");
        std::fs::write(&sibling, b"untouched").unwrap();

        let _error = write_atomic(&target, b"nope").expect_err("cannot replace a directory");
        assert!(
            target.is_dir(),
            "the directory must survive a failed replace"
        );
        assert_eq!(std::fs::read(target.join("inside")).unwrap(), b"keep");
        assert_eq!(std::fs::read(&sibling).unwrap(), b"untouched");
        assert!(temps_in(&dir).is_empty(), "staging file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Creating the temporary file is the first step. If the directory rejects that,
    /// the existing file is never opened for write.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_directory_leaves_the_old_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("locked");
        let path = dir.join("target.txt");
        std::fs::write(&path, b"intact").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let failed = write_atomic(&path, b"overwrite").is_err();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !failed {
            // Root ignores the mode bit. The directory-target test covers that case.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"intact");
        assert!(temps_in(&dir).is_empty(), "staging file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_created_restricted() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("mode");
        let path = dir.join("key.pem");
        let staged = stage(&path, b"secret", Some(0o600)).unwrap();
        assert_eq!(
            std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::rename(&staged, &path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "rename must keep the mode the temporary file was created with"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_mode_survives_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("keep-mode");
        let path = dir.join("config.toml");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, b"new").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_followed_and_keeps_the_target_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("link");
        let target = dir.join("real.toml");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink("real.toml", &link).unwrap();

        write_atomic(&link, b"new").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must not be replaced by a regular file"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(temps_in(&dir).is_empty(), "staging file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_relative_symlink_is_created_through() {
        let dir = scratch("dangling");
        let link = dir.join("config.toml");
        std::os::unix::fs::symlink("real.toml", &link).unwrap();

        write_atomic(&link, b"created").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(dir.join("real.toml")).unwrap(), b"created");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
