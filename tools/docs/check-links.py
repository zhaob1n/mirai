#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
"""Check local Markdown links and heading anchors, ignoring fenced examples.

Run from any directory: python tools/docs/check-links.py
An adjacent mirai-hmos checkout is optional and reported, never required.
"""

from collections import Counter
from pathlib import Path
from urllib.parse import unquote
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
FILES = [ROOT / "README.md", ROOT / "AGENTS.md", *sorted((ROOT / "docs").rglob("*.md"))]
LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$")
FENCE = re.compile(r"^ {0,3}(`{3,}|~{3,})")
URL = re.compile(r"^[a-z][a-z\d+.-]*:", re.I)


def scan(path):
    slugs = set()
    seen = Counter()
    links = []
    fence = None
    for line_no, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        marker = FENCE.match(line)
        if marker:
            token = marker.group(1)
            if fence is None:
                fence = (token[0], len(token))
            elif (
                token[0] == fence[0]
                and len(token) >= fence[1]
                and not line[marker.end() :].strip()
            ):
                fence = None
            continue
        if fence is not None:
            continue
        if heading := HEADING.match(line):
            title = re.sub(r"<[^>]*>", "", heading.group(1))
            title = re.sub(r"[`*_]", "", title).lower()
            slug = re.sub(r"\s+", "-", re.sub(r"[^\w\- ]", "", title).strip())
            suffix = seen[slug]
            slugs.add(slug if suffix == 0 else f"{slug}-{suffix}")
            seen[slug] += 1
        for match in LINK.finditer(line):
            target = match.group(1).strip().split(' "', 1)[0].strip("<>")
            links.append((path, line_no, target))
    return slugs, links


def main():
    headings = {}
    links = []
    for path in FILES:
        headings[path], found = scan(path)
        links.extend(found)

    errors = []
    adjacent = []
    for source, line, link in links:
        if URL.match(link) or link.startswith("//"):
            continue
        file_part, _, fragment = unquote(link).partition("#")
        target = (source.parent / file_part).resolve() if file_part else source
        location = f"{source.relative_to(ROOT)}:{line}"
        if target.is_relative_to(ROOT.parent / "mirai-hmos"):
            adjacent.append(f"{location}: optional adjacent repo: {link}")
        elif not target.exists():
            errors.append(f"{location}: missing file: {link}")
        elif fragment and target.suffix == ".md":
            if target not in headings:
                headings[target], _ = scan(target)
            if fragment not in headings[target]:
                errors.append(f"{location}: missing anchor: {link}")

    for message in (*errors, *adjacent):
        print(message)
    print(
        f"{len(FILES)} Markdown files; {len(links)} inline links; "
        f"{len(errors)} errors; {len(adjacent)} optional adjacent links"
    )
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
