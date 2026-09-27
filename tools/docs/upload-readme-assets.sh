#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Publish README images as assets of the `readme-assets` GitHub release, so they never enter
# Git history. README.md links them by file name:
#
#   https://github.com/zhaob1n/mirai/releases/download/readme-assets/<name>
#
#   tools/docs/upload-readme-assets.sh preview.png [more.png …]
#
# An asset of the same name is replaced in place, so README.md does not change. Needs `gh`
# logged in with push access; creates the release (not marked latest) on first use.
set -eu
[ $# -ge 1 ] || { sed -n '5,13p' "$0"; exit 2; }

tag=readme-assets
gh release view "$tag" >/dev/null 2>&1 ||
	gh release create "$tag" --title "README assets" --latest=false \
		--notes "Images referenced by README.md. Not a software release; assets are replaced in place with tools/docs/upload-readme-assets.sh."
gh release upload "$tag" "$@" --clobber
