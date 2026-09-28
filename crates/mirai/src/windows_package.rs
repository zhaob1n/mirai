// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! The environment the libraries in the Windows package need before they first load
//! (packaging/windows/stage.sh ships them).
//!
//! Fedora builds its MinGW libraries for the sysroot they were compiled in, and some keep
//! looking there, not beside their DLL, for what they load at run time. Each variable here
//! points one of them back at the package. A value the user set wins.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Call first thing in `main`, before any thread exists.
pub fn prepare() {
    // GStreamer derives its plugin directory from its build-time sysroot path, so it finds
    // none, and GstPlay aborts the process on the first stone sound when `playbin3` is
    // missing.
    if let Some(plugins) = package_dir(&["lib", "gstreamer-1.0"]) {
        set_default("GST_PLUGIN_SYSTEM_PATH_1_0", plugins);
    }
}

/// `parts` joined onto the package root, the parent of `bin\`, if that directory exists.
fn package_dir(parts: &[&str]) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.parent()?.to_path_buf();
    dir.extend(parts);
    dir.is_dir().then_some(dir)
}

fn set_default(var: &str, value: impl AsRef<OsStr>) {
    if std::env::var_os(var).is_none() {
        // SAFETY: `prepare` runs first thing in `main`, before any thread exists.
        unsafe { std::env::set_var(var, value) };
    }
}
