// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

//! The version a binary reports, for the `mirai` and `mirai-server` build scripts, which
//! include this file with `#[path]`.
//!
//! `Cargo.toml` names the release; `git describe` against the `v*` tags says how far past it
//! this build is: `0.1.0` on the tag, `0.1.0+21.gead50ec` 21 commits later, `0.1.0+gead50ec`
//! when no tag of this version is reachable (a shallow clone, or before the release is
//! tagged), and `0.1.0` alone outside a mirai checkout. Library crates keep
//! `CARGO_PKG_VERSION`: a build script there would rebuild every dependent on each commit.

use std::path::Path;
use std::process::Command;

/// Sets `MIRAI_VERSION` for the crate being built.
pub fn emit() {
    let cargo = std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");
    let version = from_git(&cargo).unwrap_or(cargo);
    println!("cargo:rustc-env=MIRAI_VERSION={version}");
}

fn from_git(cargo: &str) -> Option<String> {
    // An unpacked release tarball may sit inside some other repository (an AUR package
    // directory is one); its HEAD says nothing about this source.
    let package = std::env::var("CARGO_PKG_NAME").ok()?;
    if git(&["rev-parse", "--show-prefix"])? != format!("crates/{package}/") {
        return None;
    }
    watch_refs();
    let Some(described) = git(&[
        "describe",
        "--tags",
        "--long",
        "--abbrev=7",
        "--match",
        "v[0-9]*",
    ]) else {
        return Some(format!(
            "{cargo}+g{}",
            git(&["rev-parse", "--short=7", "HEAD"])?
        ));
    };
    // `v0.1.0-21-gead50ec`, or `v0.1.0-rc.1-0-gead50ec` on a prerelease tag.
    let (rest, hash) = described.rsplit_once("-g")?;
    let (tag, distance) = rest.rsplit_once('-')?;
    let tag = tag.strip_prefix('v')?;
    // The release commit bumps Cargo.toml before its tag exists; the previous tag is then
    // not this version's.
    if tag != cargo
        && !tag
            .strip_prefix(cargo)
            .is_some_and(|pre| pre.starts_with('-'))
    {
        return Some(format!("{cargo}+g{hash}"));
    }
    Some(if distance == "0" {
        tag.to_owned()
    } else {
        format!("{tag}+{distance}.g{hash}")
    })
}

/// Reruns the build script when HEAD moves or a tag changes. The working tree is not
/// watched, so uncommitted changes are not marked: that would rerun it on every edit.
fn watch_refs() {
    // All of refs/heads, not the branch's own file: `git gc` packs and deletes that file, and
    // the next commit recreates it without touching HEAD or packed-refs.
    for path in ["HEAD", "packed-refs", "refs/heads", "refs/tags"] {
        // Relative, because MSYS2's git prints an absolute path (a worktree's) as /c/…,
        // which the native build script cannot open. A path Cargo cannot find counts as
        // changed, rerunning the script on every build.
        if let Some(path) = git(&["rev-parse", "--path-format=relative", "--git-path", path])
            && Path::new(&path).exists()
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}
