// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
use std::path::Path;
use std::process::Command;

fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/mirai.gresource.xml",
        "mirai.gresource",
    );
    compile_catalogues();
    compile_builder_ui();
}

/// Blueprint files that are not templates: object trees with no widget class of their own,
/// which their caller builds on demand with `gtk::Builder::from_string` over
/// `$OUT_DIR/ui/<name>.ui`. A template compiles through `CompositeTemplate` instead; a file
/// belongs here only when there is no class to hang it on, as with an alert dialog.
const BUILDER_UI: &[&str] = &["restore_dialog", "tuning_dialog"];

fn compile_builder_ui() {
    let out = Path::new(&std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("ui");
    std::fs::create_dir_all(&out).expect("OUT_DIR is writable");
    for name in BUILDER_UI {
        let source = Path::new("src").join(format!("{name}.blp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let status = Command::new("blueprint-compiler")
            .arg("compile")
            .arg("--output")
            .arg(out.join(format!("{name}.ui")))
            .arg(&source)
            .status()
            .unwrap_or_else(|e| panic!("blueprint-compiler is needed to build mirai: {e}"));
        assert!(
            status.success(),
            "blueprint-compiler refused {}",
            source.display()
        );
    }
}

/// Compiles each language in `po/LINGUAS` into `$OUT_DIR/locale`, which a debug build run
/// from the build tree reads (`i18n::locale_dir`); `just install` compiles its own copies into
/// the prefix. `--check` refuses a translation whose placeholders differ from the English,
/// so a broken catalogue fails the build instead of showing a hole at run time.
fn compile_catalogues() {
    let po = Path::new("../../po");
    println!("cargo:rerun-if-changed={}", po.display());
    let linguas = std::fs::read_to_string(po.join("LINGUAS")).expect("po/LINGUAS is readable");
    let out = Path::new(&std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("locale");
    for lang in linguas
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|lang| !lang.is_empty())
    {
        let source = po.join(format!("{lang}.po"));
        println!("cargo:rerun-if-changed={}", source.display());
        let dir = out.join(lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&dir).expect("OUT_DIR is writable");
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg("-o")
            .arg(dir.join("mirai.mo"))
            .arg(&source)
            .status()
            .unwrap_or_else(|e| panic!("msgfmt (GNU gettext) is needed to build mirai: {e}"));
        assert!(status.success(), "msgfmt refused {}", source.display());
    }
}
