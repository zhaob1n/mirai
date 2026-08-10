// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/mirai.gresource.xml",
        "mirai.gresource",
    );
}
