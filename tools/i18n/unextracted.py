#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""List gettext calls in crates/mirai whose message xgettext left out of po/mirai.pot.

xgettext's Rust parser does not look inside a path-qualified macro such as
`glib::clone!(…)`, so a string there is silently never translated. Run by
tools/i18n/update-po.sh after it regenerates the template; exits 1 on a miss.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
CALL = re.compile(r"\b(pgettext_f|pgettext|ngettext_f|gettext_f|gettext)\(\s*")
ESCAPES = {"n": "\n", "t": "\t", "\\": "\\", '"': '"', "'": "'", "0": "\0"}


def rust_string(text, i):
    """The Rust string literal starting at text[i], and the index after it."""
    out, i = [], i + 1
    while text[i] != '"':
        if text[i] != "\\":
            out.append(text[i])
            i += 1
        elif text[i + 1] == "\n":  # a continuation also swallows the next line's indent
            i += 2
            while text[i] in " \t\n":
                i += 1
        elif text[i + 1] == "u":
            end = text.index("}", i)
            out.append(chr(int(text[i + 3 : end], 16)))
            i = end + 1
        else:
            out.append(ESCAPES[text[i + 1]])
            i += 2
    return "".join(out), i + 1


def po_string(line):
    return re.sub(r'\\(.)', lambda m: ESCAPES.get(m.group(1), m.group(1)), line[1:-1])


def template_messages(pot):
    messages, context, key = set(), None, None
    for line in pot.read_text().splitlines() + [""]:
        if line.startswith("msgctxt "):
            context, key = po_string(line[8:]), "ctx"
        elif line.startswith("msgid "):
            msgid, key = po_string(line[6:]), "id"
        elif line.startswith('"') and key == "id":
            msgid += po_string(line)
        elif line.startswith('"') and key == "ctx":
            context += po_string(line)
        elif line.startswith("msgid_plural") or line.startswith("msgstr"):
            if key == "id":
                messages.add((context, msgid))
            context, key = None, None
    return messages


def main():
    known = template_messages(ROOT / "po" / "mirai.pot")
    missing = 0
    for path in sorted((ROOT / "crates" / "mirai" / "src").rglob("*.rs")):
        text = path.read_text()
        for call in CALL.finditer(text):
            i = call.end()
            if text[i] != '"':
                continue
            first, i = rust_string(text, i)
            context, msgid = None, first
            if call.group(1).startswith("pgettext"):
                comma = re.match(r"\s*,\s*", text[i:])
                context, (msgid, _) = first, rust_string(text, i + comma.end())
            if (context, msgid) not in known:
                line = text.count("\n", 0, call.start()) + 1
                print(f"{path.relative_to(ROOT)}:{line}: not extracted: {msgid!r}")
                missing += 1
    return 1 if missing else 0


if __name__ == "__main__":
    sys.exit(main())
