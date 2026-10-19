// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin

#[path = "../../tools/build/version.rs"]
mod version;

fn main() {
    // Without it, a build outside a git checkout reruns this on any change in the package.
    println!("cargo:rerun-if-changed=build.rs");
    version::emit();
}
