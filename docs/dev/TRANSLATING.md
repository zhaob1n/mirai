<!--
SPDX-License-Identifier: GPL-3.0-or-later
Copyright (C) 2026 Huang Zhaobin
-->

# Translating mirai

mirai is translated with GNU gettext, as GNOME applications are. The interface, the desktop
entry and the AppStream metainfo share one catalogue per language in `po/`. User documents
are translated as separate files (`README.zh-CN.md`, `docs/user/GUIDE.zh-CN.md`); developer
documents are written in English, except the Chinese API notes for the record servers:
[`FOX_KIFU_API_SPEC.md`](FOX_KIFU_API_SPEC.md), [`EWEIQI_KIFU_API_SPEC.md`](EWEIQI_KIFU_API_SPEC.md)
and [`YIKE_KIFU_API_SPEC.md`](YIKE_KIFU_API_SPEC.md).

## Adding or updating a language

```sh
tools/i18n/update-po.sh                          # regenerate po/mirai.pot, merge every po/*.po
msginit -i po/mirai.pot -l fr_FR.UTF-8 -o po/fr.po   # a new language, then add it to po/LINGUAS
LANGUAGE=fr cargo run -p mirai                   # see it; build.rs compiled the catalogue
```

Edit the `.po` file with any PO editor (GNOME Translation Editor, Poedit, Lokalize) or by
hand. `po/mirai.pot` is derived from the sources and not tracked; regenerate it rather than
editing it. Put your name in the `translator-credits` message: the About dialog shows it.

Run `tools/i18n/update-po.sh` again before committing a catalogue. It keeps each one in a
form whose diff shows only what changed in its messages: references name the file without a
line, no line is wrapped, and messages the sources dropped go instead of lingering as `#~`.
An editor that wraps or reorders lines is put back, and a catalogue whose messages did not
change is left untouched, creation date and all. Messages stay in source order, which shows
a translator a message's neighbours, so moving code still moves its messages.

`build.rs` runs `msgfmt --check` on every language in `po/LINGUAS`, so a translation that
drops or renames a `{placeholder}`, or breaks the file, fails the build instead of showing a
hole. `msgfmt --statistics po/fr.po -o /dev/null` counts what is left to do.

A message marked with a context (`msgctxt "player"`, `"column"`, `"verb"`) is a short string
whose meaning its words alone do not settle; `#.` lines above a message are notes from the
developers. Keep the `_` mnemonic of menu items: GNOME's CJK convention appends it, as in
`打开文件(_O)…`.

## Marking strings in the code

Only `crates/mirai` is translated. All UI strings, including fixed layouts, go through
`crates/mirai/src/i18n.rs`.

| Need | Write |
|---|---|
| a string | `gettext("Save As…")` |
| values in it | `gettext_f("Could not open: {error}", &[("error", &e.to_string())])` |
| a count | `ngettext_f("{n} move", "{n} moves", n as u64, &[("n", &n.to_string())])` |
| a short, ambiguous word | `pgettext("verb", "Pass")` |
| an ambiguous string with values | `pgettext_f("records", "{first}–{last} of {count}", &[…])` |
| a note for translators | `// Translators: …` on the line above |

- Pass the literal straight to the function: `xgettext` extracts nothing else. It also skips
  the inside of a path-qualified macro, so write `clone!(…)`, not `glib::clone!(…)`;
  `tools/i18n/update-po.sh` fails on any call it could not see.
- One sentence is one message. Never `format!` translated fragments together; name the
  placeholders so a translator can reorder them.
- Write a sentence whose subject is a colour once per colour ("Black to play", "White to
  play"); `i18n::color_name` is for a colour standing alone.
- Leave untranslated: identifiers, config keys, log lines, `expect` messages, and the details
  of an error from another crate or the OS, which go into a translated sentence as a value.
- `mirai-core` and `mirai-client` link no GTK and so no gettext, and they word nothing for a
  window: a value leaves them as data. The GTK window words it through `i18n::rules_label`,
  `i18n::result_phrase` and the structured `play::outcome`, `Play::end`, `Play::count` and
  `PlayError`; the HarmonyOS client words the same data in its own resources. A new
  user-visible value from those crates needs the same treatment: expose it as data, word it
  in `crates/mirai`.
- Text drawn in `snapshot()` is translated when the data changes, not per frame (INV-9).

## How a build finds its catalogues

`i18n::init`, first thing in `main`, binds the `mirai` domain to `<prefix>/share/locale` next
to an installed `<prefix>/bin/mirai`, whatever the prefix, so installing needs no build-time
path. A debug build in the build tree has no such directory and reads the catalogues
`build.rs` compiled into `OUT_DIR`, which is why `cargo run` is translated too; a release
build carries no build path, so one run from the tree is English. `just install` compiles
its own copies into the prefix and merges the translations into the desktop entry and
metainfo from `data/*.in` with `msgfmt --desktop` and `--xml`.

Lookups go through GLib's `g_dgettext`, as GtkBuilder's do for the templates: in a language
mirai has no catalogue for, GTK's own strings stay English too rather than mixing two
languages in one window.

## Simplified Chinese

`po/zh_CN.po` follows the GNOME Chinese team's conventions: full-width punctuation in Chinese
sentences, a half-width space between Chinese and Latin letters or digits, `(_X)` mnemonics.
The Go terms are the ones Chinese players use, and the Chinese user guide quotes the same
labels:

| English | 中文 | English | 中文 |
|---|---|---|---|
| win rate | 胜率 | score lead | 目差 |
| visits | 计算量 | candidate | 候选手 |
| blunder | 问题手 | ownership / policy | 地盘 / 策略 |
| live analysis | 实时分析 | whole-game analysis | 全盘分析 |
| variation / main line | 变化 / 主线 | move tree | 棋谱树 |
| game record | 棋谱 | engine profile | 引擎配置 |
| pass / resign | 停一手 / 认输 | komi / handicap | 贴目 / 让子 |
| byo-yomi / absolute | 读秒 / 包干计时 | estimate score | 形势判断 |
| Fox | 野狐 | Black / White | 黑方 / 白方 |
| eWeiqi / Yike | 弈城 / 弈客 | download game record | 下载棋谱 |
