// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Huang Zhaobin
//! Translation through gettext, the way GNOME applications do it.
//!
//! [`init`] binds the `mirai` catalogue before anything is shown. Lookups go through GLib's
//! `g_dgettext` family, the same path GtkBuilder takes for the Blueprint templates'
//! `_("…")` strings, so a language mirai has no catalogue for leaves GTK's own strings
//! untranslated too instead of mixing languages in one window.
//!
//! Every function here is an `xgettext` keyword (`tools/i18n/update-po.sh`). A string that
//! takes values uses a named `{placeholder}` and a `_f` function, never `format!` over a
//! translated fragment: a translator must see the whole sentence and may reorder it.
//! `msgfmt --check` (run by `build.rs`) refuses a translation that drops or renames one.

use std::ffi::{CString, c_char, c_int};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use mirai_client::play::{Outcome, outcome};
use mirai_core::{Color, IllegalMove, RuleSet};
use mirai_engine::EngineError;

const DOMAIN: &std::ffi::CStr = c"mirai";

// libintl is part of glibc, so these link with nothing extra; the workspace takes no `libc`
// dependency (as in `mirai_proto::atomic`). A port to a C library without libintl links it.
unsafe extern "C" {
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
    fn bindtextdomain(domain: *const c_char, dir: *const c_char) -> *mut c_char;
    fn bind_textdomain_codeset(domain: *const c_char, codeset: *const c_char) -> *mut c_char;
    fn textdomain(domain: *const c_char) -> *mut c_char;
}

/// `LC_ALL` as glibc and musl number it.
const LC_ALL: c_int = 6;

/// Adopts the user's locale and binds the catalogue. Call first thing in `main`: GLib decides
/// once, at the first lookup, whether translating is wanted at all.
pub fn init() {
    let dir = CString::new(locale_dir().into_os_string().into_vec())
        .expect("a filesystem path holds no NUL");
    // SAFETY: every pointer is a NUL-terminated string that outlives the call; libintl copies
    // what it keeps. A locale the C library lacks makes `setlocale` return NULL and leaves
    // the "C" locale, which only means English.
    unsafe {
        setlocale(LC_ALL, c"".as_ptr());
        bindtextdomain(DOMAIN.as_ptr(), dir.as_ptr());
        bind_textdomain_codeset(DOMAIN.as_ptr(), c"UTF-8".as_ptr());
        textdomain(DOMAIN.as_ptr());
    }
}

/// `<prefix>/share/locale` beside an installed `<prefix>/bin/mirai`, whatever the prefix, so
/// installing needs no build-time path. A binary run from the build tree has no such
/// directory and reads the catalogues `build.rs` compiled for this build instead.
fn locale_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")))
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(|| PathBuf::from(concat!(env!("OUT_DIR"), "/locale")))
}

pub fn gettext(msgid: &str) -> String {
    glib::dgettext(None, msgid).into()
}

/// [`gettext`] for a string whose meaning its words alone do not settle ("Pass" the verb
/// or the noun), told apart by `context`.
pub fn pgettext(context: &str, msgid: &str) -> String {
    glib::dpgettext2(None, context, msgid).into()
}

/// [`gettext`], then each `{name}` in the translation replaced by its value in `args`.
pub fn gettext_f(msgid: &str, args: &[(&str, &str)]) -> String {
    fill(&glib::dgettext(None, msgid), args)
}

/// `ngettext` with placeholders, as [`gettext_f`]: the count appears as one of them, and `n`
/// chooses the plural form.
pub fn ngettext_f(msgid: &str, msgid_plural: &str, n: u64, args: &[(&str, &str)]) -> String {
    fill(&glib::dngettext(None, msgid, msgid_plural, n as _), args)
}

/// One pass over `template`, so a value that itself contains `{name}` is not expanded
/// again. Braces that name no argument are kept as written.
fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out =
        String::with_capacity(template.len() + args.iter().map(|a| a.1.len()).sum::<usize>());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let value = rest[1..].find('}').and_then(|close| {
            let key = &rest[1..1 + close];
            let (_, value) = args.iter().find(|(k, _)| *k == key)?;
            Some((value, close + 2))
        });
        match value {
            Some((value, len)) => {
                out.push_str(value);
                rest = &rest[len..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

// -- what the lower crates spell in English ---------------------------------------------
//
// `mirai-core` and `mirai-client` link no GTK and so no gettext; their `label()`, `name()`
// and prose stay English for the HarmonyOS client. This window words the same values here.

/// A colour standing alone, as a player: "Black". A sentence about a colour is written
/// out once per colour instead, so a translation can inflect it.
pub fn color_name(color: Color) -> String {
    match color {
        Color::Black => pgettext("player", "Black"),
        Color::White => pgettext("player", "White"),
    }
}

pub fn rules_label(rules: RuleSet) -> String {
    match rules {
        RuleSet::TrompTaylor => gettext("Tromp-Taylor"),
        RuleSet::Chinese => gettext("Chinese"),
        RuleSet::ChineseOgs => gettext("Chinese (OGS)"),
        RuleSet::Japanese => gettext("Japanese"),
        RuleSet::Korean => gettext("Korean"),
        RuleSet::StoneScoring => gettext("Stone Scoring"),
        RuleSet::Aga => gettext("AGA"),
        // Translators: AGA rules with the pass button, a stone handed over for the first
        // pass.
        RuleSet::AgaButton => gettext("AGA (Button)"),
        RuleSet::NewZealand => gettext("New Zealand"),
    }
}

/// An SGF result (`"B+R"`, `"W+7.5"`) as a sentence.
pub fn result_phrase(result: &str) -> String {
    match outcome(result) {
        Outcome::None => gettext("No result"),
        // Translators: a drawn game of Go.
        Outcome::Draw => gettext("Jigo — a draw"),
        Outcome::Resignation(Color::Black) => gettext("Black wins by resignation"),
        Outcome::Resignation(Color::White) => gettext("White wins by resignation"),
        Outcome::Time(Color::Black) => gettext("Black wins on time"),
        Outcome::Time(Color::White) => gettext("White wins on time"),
        Outcome::Forfeit(Color::Black) => gettext("Black wins by forfeit"),
        Outcome::Forfeit(Color::White) => gettext("White wins by forfeit"),
        // Translators: {margin} is a number of points, such as 7.5.
        Outcome::Points(Color::Black, margin) => {
            gettext_f("Black wins by {margin}", &[("margin", margin)])
        }
        // Translators: {margin} is a number of points, such as 7.5.
        Outcome::Points(Color::White, margin) => {
            gettext_f("White wins by {margin}", &[("margin", margin)])
        }
        Outcome::Win(Color::Black) => gettext("Black wins"),
        Outcome::Win(Color::White) => gettext("White wins"),
        Outcome::Other(text) => text.to_string(),
    }
}

pub fn illegal_move(error: IllegalMove) -> String {
    match error {
        IllegalMove::Occupied => gettext("That point is already occupied"),
        IllegalMove::Suicide => gettext("That move would be suicide"),
        IllegalMove::Ko => gettext("That move breaks the ko rule"),
        IllegalMove::OffBoard => gettext("That point is off the board"),
    }
}

/// An engine error in its own words, with the engine's or the system's detail left as
/// written.
pub fn engine_error(error: &EngineError) -> String {
    // Translators: in the engine errors below, {error} is KataGo's, the server's or the
    // system's own message, which stays untranslated.
    let (msgid, detail) = match error {
        EngineError::Startup(detail) => (gettext("engine failed to start: {error}"), detail),
        EngineError::EngineExited(detail) => (gettext("engine process exited: {error}"), detail),
        EngineError::Query(detail) => (gettext("query rejected: {error}"), detail),
        EngineError::Disconnected(detail) => {
            (gettext("disconnected from remote engine: {error}"), detail)
        }
        EngineError::Protocol(detail) => (gettext("protocol error: {error}"), detail),
        EngineError::Other(detail) => return detail.clone(),
    };
    fill(&msgid, &[("error", detail)])
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn placeholders_follow_the_translation_and_expand_once() {
        // A translation may reorder placeholders; a value is inserted verbatim even when it
        // looks like another placeholder.
        assert_eq!(
            fill("{b} 对 {a}", &[("a", "{b}"), ("b", "Kata")]),
            "Kata 对 {b}"
        );
        assert_eq!(fill("{x} {} {", &[("a", "1")]), "{x} {} {");
    }
}
