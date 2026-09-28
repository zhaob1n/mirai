#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Regenerate po/mirai.pot from the sources, then merge it into every language in po/LINGUAS
# (docs/dev/TRANSLATING.md). The template is derived data and is not tracked.
#
#   tools/i18n/update-po.sh
set -eu
cd "$(dirname "$0")/../.."
pot=po/mirai.pot

# Every GUI source that calls a gettext function.
rust=$(find crates/mirai/src -name '*.rs' -print0 | sort -z |
    xargs -0 grep -lE '\b[np]?gettext(_f)?\(')
blp=$(find crates/mirai/src -name '*.blp' | sort)

set -- --package-name=mirai --msgid-bugs-address=https://github.com/zhaob1n/mirai/issues \
    --from-code=UTF-8 --add-comments=Translators: -o "$pot"
# shellcheck disable=SC2086 # the file lists are space-free paths
xgettext "$@" -L Rust -k -kgettext -kpgettext:1c,2 -kgettext_f -kngettext_f:1,2 \
    $rust
# Blueprint's `_("…")` and `C_("context", "…")` lex as C, as blueprint-compiler documents.
# shellcheck disable=SC2086
xgettext "$@" -j -L C -k -k_ -kC_:1c,2 $blp
# The desktop entry's Name is the brand, left as it is.
xgettext "$@" -j -k -kGenericName -kComment -kKeywords data/io.github.zhaob1n.Mirai.desktop.in
xgettext "$@" -j data/io.github.zhaob1n.Mirai.metainfo.xml.in

for lang in $(sed 's/#.*//' po/LINGUAS); do
    msgmerge --quiet --update --backup=none --previous "po/$lang.po" "$pot"
done

# A string xgettext could not see would stay English without a word; refuse that.
tools/i18n/unextracted.py
