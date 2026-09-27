<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Translating mirai

mirai is translated with GNU gettext, as GNOME applications are. The interface has one
catalogue per language in `po/`; developer documents stay English.

## Adding or updating a language

```sh
tools/i18n/update-po.sh                          # regenerate po/mirai.pot, merge every po/*.po
msginit -i po/mirai.pot -l fr_FR.UTF-8 -o po/fr.po   # a new language, then add it to po/LINGUAS
LANGUAGE=fr cargo run -p mirai                   # see it; build.rs compiled the catalogue
```

Edit the `.po` file with any PO editor (GNOME Translation Editor, Poedit, Lokalize) or by
hand. `po/mirai.pot` is derived from the sources and not tracked; regenerate it rather than
editing it. Put your name in the `translator-credits` message: the About dialog shows it.

`build.rs` runs `msgfmt --check` on every language in `po/LINGUAS`, so a translation that
drops or renames a `{placeholder}`, or breaks the file, fails the build instead of showing a
hole. `msgfmt --statistics po/fr.po -o /dev/null` counts what is left to do.

A message marked with a context (`msgctxt "player"`, `"column"`, `"verb"`) is a short string
whose meaning its words alone do not settle; `#.` lines above a message are notes from the
developers. Keep the `_` mnemonic of menu items: GNOME's CJK convention appends it, as in
`打开文件(_O)…`.

## Marking strings in the code

Only `crates/mirai` is translated. Its strings go through `crates/mirai/src/i18n.rs`; the
Blueprint templates use `_("…")` and `C_("context", "…")`.

| Need | Write |
|---|---|
| a string | `gettext("Save As…")` |
| values in it | `gettext_f("Could not open: {error}", &[("error", &e.to_string())])` |
| a count | `ngettext_f("{n} move", "{n} moves", n as u64, &[("n", &n.to_string())])` |
| a short, ambiguous word | `pgettext("verb", "Pass")` |
| a note for translators | `// Translators: …` on the line above (`/* Translators: … */` in Blueprint) |

- Pass the literal straight to the function: `xgettext` extracts nothing else. It also skips
  the inside of a path-qualified macro, so write `clone!(…)`, not `glib::clone!(…)`;
  `tools/i18n/update-po.sh` fails on any call it could not see.
- One sentence is one message. Never `format!` translated fragments together; name the
  placeholders so a translator can reorder them.
- Write a sentence whose subject is a colour once per colour ("Black to play", "White to
  play"); `i18n::color_name` is for a colour standing alone.
- Leave untranslated: identifiers, config keys, log lines, `expect` messages, and the details
  of an error from another crate or the OS, which go into a translated sentence as a value.
- `mirai-core` and `mirai-client` link no GTK and so no gettext. Their English (`RuleSet::label`,
  `Color::name`, `play::result_phrase`, `Play::summary`) is for the HarmonyOS client; the GTK
  window words the same values through `i18n::rules_label`, `i18n::result_phrase` and the
  structured `Play::end`, `Play::count` and `PlayError`. A new user-visible value from those
  crates needs the same treatment: expose it as data, word it in `crates/mirai`.
- Text drawn in `snapshot()` is translated when the data changes, not per frame (INV-9).

## How a build finds its catalogues

`i18n::init`, first thing in `main`, binds the `mirai` domain to `<prefix>/share/locale` next
to an installed `<prefix>/bin/mirai`, whatever the prefix, so installing needs no build-time
path. A binary in the build tree has no such directory and reads the catalogues `build.rs`
compiled into `OUT_DIR`, which is why `cargo run` is translated too.

Lookups go through GLib's `g_dgettext`, as GtkBuilder's do for the templates: in a language
mirai has no catalogue for, GTK's own strings stay English too rather than mixing two
languages in one window.
