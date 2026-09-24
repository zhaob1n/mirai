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

/// How many names `stage` tries before giving up. Only a crashed process that held this
/// pid can have left one of ours behind, so a handful covers any real directory.
const STAGE_ATTEMPTS: u32 = 16;

/// A sibling of `target` whose name includes this process's pid and sequence number `n`.
/// Hidden by a leading dot so a crash between create and rename does not look like a
/// second config.
fn temp_path(target: &Path, n: u64) -> PathBuf {
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
/// mode; `std::fs::write` did both. That suits a file the user placed and may have
/// linked elsewhere. A file that holds secrets goes through [`write_atomic_private`]; a
/// file mirai names and generates itself goes through [`write_atomic_generated`].
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, None, Links::Follow)
}

/// [`write_atomic`] for a file mirai generates under a name of its own choosing, such as
/// KataGo's analysis config.
///
/// A symlink at `path` is replaced by the new file, never written through. Whoever can
/// plant a link under a predictable name would otherwise choose which file this write
/// clobbers, and resolving the link here, in user space, sidesteps the kernel's
/// `fs.protected_symlinks` guard. An existing regular file keeps its mode.
pub fn write_atomic_generated(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, None, Links::Replace)
}

/// Whether [`replace`] writes through a symlink at the target or replaces the link.
#[derive(Clone, Copy)]
enum Links {
    Follow,
    Replace,
}

/// Creates `dir`, and any missing parent, readable only by this user, and refuses an
/// existing directory that another user owns or can write to.
///
/// mirai writes generated files into such a directory and KataGo reads them back later.
/// `create_dir_all` happily accepts a directory another local user created first under a
/// predictable name, and whoever can write to the directory can swap a file in it
/// between mirai's write and KataGo's read. On other platforms the directory is only
/// created.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        check_private_dir(dir)
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// The ownership and mode half of [`private_dir`]. A symlinked directory is accepted only
/// when the link is this user's too: a link someone else owns can be repointed after the
/// check.
#[cfg(unix)]
fn check_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let me = euid();
    let mut meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        if meta.uid() != me {
            return Err(refuse(
                dir,
                &format!(
                    "it is a symlink owned by uid {}, not this user (uid {me})",
                    meta.uid()
                ),
            ));
        }
        meta = std::fs::metadata(dir)?;
    }
    if meta.uid() != me {
        return Err(refuse(
            dir,
            &format!(
                "it is owned by uid {}, not this user (uid {me})",
                meta.uid()
            ),
        ));
    }
    let mode = meta.mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(refuse(
            dir,
            &format!("its mode {mode:04o} lets other users write to it"),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn refuse(dir: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refusing {}: {why}, so another user could replace the files mirai generates \
             there; use a directory only you can write to",
            dir.display()
        ),
    )
}

/// This process's effective uid. std exposes none and the workspace takes no `libc`
/// dependency; `geteuid` is always successful.
#[cfg(unix)]
fn euid() -> u32 {
    unsafe extern "C" {
        safe fn geteuid() -> u32;
    }
    geteuid()
}

/// [`write_atomic`] for a file only its owner may read, such as a config holding remote
/// bearer tokens: on Unix the result is `0600` whether the file is new or not, so an
/// existing file left wider — by the umask when it was first written, or by hand — is
/// tightened by the next write. The temporary file is created `0600`, so the bytes are
/// never readable by anyone else, not even while they are staged.
pub fn write_atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    replace(path, bytes, Some(0o600), Links::Follow)
}

/// Stages `bytes` beside `target` and returns the temporary path. The caller renames it
/// into place. The certificate installer uses this so a key and a cert can land in a
/// fixed order instead of each rename racing the other file.
///
/// On Unix, `mode` is the permission the temporary file is created with, so a private
/// key is `0600` from the moment it exists rather than after a later `chmod`. Other
/// platforms ignore it.
pub(crate) fn stage(target: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<PathBuf> {
    stage_with(target, bytes, mode, || {
        TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
    })
}

/// [`stage`] with the sequence source passed in, so a test can aim at a name it has
/// already occupied.
///
/// A process that crashed between staging and renaming leaves its temporary file
/// behind, and a later process that is given the same pid would pick the same names.
/// `AlreadyExists` therefore means "try the next number", never "the write failed" — and
/// the file that was in the way is not ours to delete.
fn stage_with(
    target: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    mut next: impl FnMut() -> u64,
) -> io::Result<PathBuf> {
    for _ in 0..STAGE_ATTEMPTS {
        let tmp = temp_path(target, next());
        let file = match create_new(&tmp, mode) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        write_synced(file, &tmp, bytes)?;
        return Ok(tmp);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free temporary name beside {}", target.display()),
    ))
}

/// Writes and syncs a freshly created temporary file, deleting it on failure.
fn write_synced(mut file: File, path: &Path, bytes: &[u8]) -> io::Result<()> {
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

fn replace(path: &Path, bytes: &[u8], mode: Option<u32>, links: Links) -> io::Result<()> {
    let target = match links {
        Links::Follow => follow(path)?,
        Links::Replace => path.to_path_buf(),
    };
    let existing = existing_permissions(&target)?;
    // Create the temporary file no wider than the file it replaces, or at the mode asked
    // for. Staging with the umask default and narrowing afterwards left a private file
    // readable by other users for as long as the write took. The umask can only narrow
    // these bits further; the exact ones are set before the rename.
    let create_mode = mode.or_else(|| existing.as_ref().and_then(unix_mode));
    let staged = stage(&target, bytes, create_mode)?;
    let exact = match mode {
        Some(mode) => mode_permissions(mode),
        None => existing,
    };
    if let Some(permissions) = exact
        && let Err(error) = std::fs::set_permissions(&staged, permissions)
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

/// The permissions of the regular file about to be replaced; `None` if there is none
/// yet, in which case the new file gets the umask default exactly as `std::fs::write`
/// gave it. A symlink left at `target` is about to be replaced rather than followed, so
/// its target's mode is none of this file's business.
fn existing_permissions(target: &Path) -> io::Result<Option<std::fs::Permissions>> {
    match std::fs::symlink_metadata(target) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(None),
        Ok(meta) => Ok(Some(meta.permissions())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn unix_mode(permissions: &std::fs::Permissions) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(permissions.mode() & 0o7777)
}

#[cfg(not(unix))]
fn unix_mode(_permissions: &std::fs::Permissions) -> Option<u32> {
    None
}

#[cfg(unix)]
fn mode_permissions(mode: u32) -> Option<std::fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    Some(std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn mode_permissions(_mode: u32) -> Option<std::fs::Permissions> {
    None
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

    /// A process that crashed mid-write leaves its temporary file; a later process given
    /// the same pid picks the same names. That leftover must be stepped over and kept,
    /// not turn every save of the target into `AlreadyExists`.
    #[test]
    fn a_leftover_temporary_name_is_skipped_not_fatal() {
        let dir = scratch("stale");
        let target = dir.join("config.toml");
        let stale = temp_path(&target, 0);
        std::fs::write(&stale, b"a crashed write").unwrap();

        let mut seq = 0..;
        let staged = stage_with(&target, b"new", None, || seq.next().unwrap()).unwrap();

        assert_eq!(staged, temp_path(&target, 1));
        assert_eq!(std::fs::read(&staged).unwrap(), b"new");
        assert_eq!(std::fs::read(&stale).unwrap(), b"a crashed write");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Another local user who can guess a generated file's name plants a link there to a
    /// file this user can write. The generated write must replace the link, leaving the
    /// file it pointed at alone.
    #[cfg(unix)]
    #[test]
    fn a_generated_write_replaces_a_planted_symlink() {
        let dir = scratch("planted");
        let victim = dir.join("victim");
        std::fs::write(&victim, b"precious").unwrap();
        let generated = dir.join("katago-analysis.cfg");
        std::os::unix::fs::symlink(&victim, &generated).unwrap();

        write_atomic_generated(&generated, b"generated").unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        let meta = std::fs::symlink_metadata(&generated).unwrap();
        assert!(
            meta.is_file(),
            "the link must be replaced by a regular file"
        );
        assert_eq!(std::fs::read(&generated).unwrap(), b"generated");
        assert!(temps_in(&dir).is_empty(), "staging file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_private_dir_is_created_for_this_user_alone() {
        use std::os::unix::fs::PermissionsExt;

        let base = scratch("private-new");
        let dir = base.join("state").join("katago-logs");
        private_dir(&dir).unwrap();
        for created in [&dir, &base.join("state")] {
            assert_eq!(
                std::fs::metadata(created).unwrap().permissions().mode() & 0o777,
                0o700,
                "{} must be private",
                created.display()
            );
        }
        // An existing directory of this user's that nobody else can write stays usable.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// `/tmp/mirai-katago-logs` created world-writable by whoever got there first must
    /// not be written into.
    #[cfg(unix)]
    #[test]
    fn a_directory_others_can_write_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch("shared");
        for mode in [0o777, 0o1777, 0o770] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let error = private_dir(&dir).expect_err("a directory others can write is refused");
            assert_eq!(
                error.kind(),
                io::ErrorKind::PermissionDenied,
                "mode {mode:o}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
