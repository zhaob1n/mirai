<div align="center">

<img src="crates/mirai/resources/icons/hicolor/scalable/apps/io.github.zhaob1n.Mirai.svg" width="96" alt="">

# mirai

**Go analysis, review and play with KataGo, made for the GNOME desktop.**

English | [简体中文](README.zh-CN.md)
</div>

![mirai reviewing a game with live KataGo analysis](https://github.com/zhaob1n/mirai/releases/download/readme-assets/preview.png)

mirai puts KataGo's reading on a board that feels at home on Linux. Every candidate move
tells you at a glance how much it loses and how far to trust that number; a win-rate graph
marks each blunder of a game; a move tree keeps every variation you try. Play the engine at
any strength, or pull a game from Fox, eWeiqi or Yike and replay it with KataGo beside you.

The engine does not have to be on the same computer. When another machine has more compute,
or you need to analyse remotely, run `mirai-server` there and connect over the network.

---

## ✨ Highlights

- 🎯 **Readable analysis.** Each candidate shows win rate, score lead and visits. Its colour is
  what it loses against the engine's pick, from cyan through green and yellow to red; how
  solid it is says how much search stands behind it. Hover one to see its variation played
  out on the board, without touching the record.
- 📈 **Whole-game review.** One key sweeps the main line. The win-rate and score-lead curves
  fill in, a strip under the graph marks every mistake from the side that made it, and a
  Blunders list jumps straight to the move.
- 🗺️ **Ownership and policy overlays** show who KataGo expects to own each point, and where
  the raw network wanted to play before any search.
- ✏️ **A real SGF editor.** Variations, setup stones, marks, labels and comments, with undo and
  redo. Multi-game collections open; properties mirai does not draw are kept, so other
  programs' files survive a round trip.
- ⚫ **Play against KataGo** by visits, by time per move, or at a human-like rank with a human
  SL network. Boards from 2×2 to 19×19, handicap, nine rulesets, absolute, byo-yomi or
  Fischer clocks. After two passes KataGo marks the dead stones, and a click fixes any group
  it misjudged. Or play both sides yourself, with no engine at all.
- 🔎 **Online records.** Look up a player on Fox (nickname or UID), eWeiqi (name or nickname,
  in its tournament catalogue) or Yike (nickname, account or professional's name) and open
  any of their latest public games.
- 🔒 **A remote engine that stays private.** `mirai-server` shares one or more KataGo
  instances with every client on your network over QUIC. Clients authenticate with a token
  and pin the server's certificate the first time they connect.
- ⚡ **Native and smooth.** GTK 4 and libadwaita, light and dark styles, and rendering
  specially tuned to stay fluid even on high-refresh displays. Several windows share one
  KataGo, and autosave brings your record back after a crash.
- 🌏 **In your language.** English and Simplified Chinese so far; translations are welcome.

## 📦 Installing

### Arch Linux

Install [`mirai-git`](https://aur.archlinux.org/packages/mirai-git) from the AUR, and
[`mirai-server-git`](https://aur.archlinux.org/packages/mirai-server-git) on a machine that
should share its KataGo. Both build the latest commit.

### From source

- The current stable Rust (edition 2024), which `rust-toolchain.toml` selects. No older
  compiler is supported.
- GTK 4.22+, libadwaita 1.9+ and libsoup 3, with their development packages.
- GNU gettext, for the translations.
- [`just`](https://github.com/casey/just), to install.

|Distribution|Packages|
|---|---|
|Arch|`gtk4 libadwaita libsoup3 gettext just`|
|Debian / Ubuntu|`libgtk-4-dev libadwaita-1-dev libsoup-3.0-dev gettext just`|
|Fedora|`gtk4-devel libadwaita-devel libsoup3-devel gettext just`|

A distribution release older than GNOME 50 ships a GTK and libadwaita too old to build mirai.

```
just build
sudo just install        # both; or `just install mirai` / `just install mirai-server`
sudo just uninstall      # likewise
```

This installs `mirai`, `mirai-server`, the desktop entry, metainfo, icons and translations
under `/usr/local`; `just prefix=$HOME/.local install` needs no root.

For stone sounds, install GStreamer's good plugins (`gst-plugins-good` on Arch,
`gstreamer1.0-plugins-good` on Debian/Ubuntu, `gstreamer1-plugins-good` on Fedora),
which GTK plays audio through; without them mirai runs silently.

### Windows installer

The [Windows release workflow](.github/workflows/windows-release.yml) produces
`mirai-VERSION-windows-x86_64-setup.exe`. When attached to a GitHub release, it installs
per user without MSYS2 or administrator rights; GTK and its runtime are included.
KataGo and its model are separate. The installer is unsigned, so Windows may show a
SmartScreen warning. Uninstall leaves settings and game records untouched.

<a id="windows-msys2-ucrt64"></a>

### Windows (MSYS2 UCRT64)

A native build uses an [MSYS2](https://www.msys2.org/) **UCRT64** shell, not MSYS, CLANG64 or
MINGW64, and not rustup. UCRT64's rustc host is `x86_64-pc-windows-gnu` (GCC and UCRT).
CLANG64's is `x86_64-pc-windows-gnullvm`. `rust-toolchain.toml` makes a rustup proxy ignore
MSYS2's compiler.

In that shell, from [`packaging/msys2/mirai-git`](packaging/msys2/mirai-git), and from
[`packaging/msys2/mirai-server-git`](packaging/msys2/mirai-server-git) on a machine that should
share KataGo (that package does not install GTK):

```
pacman -S --needed git
makepkg-mingw -si
```

`makepkg-mingw` installs the build dependencies and the package under `/ucrt64`. The GUI
package requires GTK 4.22 and libadwaita 1.9, the same floor as a source build. KataGo and its
model are not in the package. For stone sounds, install
`mingw-w64-ucrt-x86_64-gst-plugins-good` (WAV decoding) and
`mingw-w64-ucrt-x86_64-gst-plugins-bad` (WASAPI2 output). The installer includes both
required plugin subsets.

Expect stutter, and do not treat a Wine run as a Windows test. GTK's Windows backend is less
mature than its Linux ones: its GPU renderers draw a black band around every window, so GTK
4.24 draws with Cairo on the CPU, and scrolling, resizing and animation can hitch. That is
not fixed. Native Windows CI covers the suite, installation/runtime/uninstall and an English
GUI screenshot. Desktop GPU performance, Chinese text rendering and audio-device output
remain unverified.

From a checkout, with the package's build dependencies installed and `/ucrt64/bin`
ahead of any rustup:

```
cargo build --locked --release --target x86_64-pc-windows-gnu -p mirai -p mirai-server
MSYS2_ENV_CONV_EXCL=PREFIX PREFIX=/ucrt64 MIRAI_EXE_SUFFIX=.exe CARGO_BUILD_TARGET=x86_64-pc-windows-gnu just install
```

## 🚀 Getting started

```
mirai
mirai game.sgf
```

From a source checkout, `cargo run -p mirai -- game.sgf` does the same.

### Engine setup

To analyse or play against the AI, mirai connects to KataGo — either locally or over the
network via [`mirai-server`](#-running-over-a-network).

For local analysis, supply a KataGo binary (JSON analysis mode, not GTP) and a neural
network model (`*.bin.gz`); mirai does not download them. If `katago` is in your `PATH`
and a model is in standard locations (`~/.local/share/katago/models/` or `~/.katago/models/`),
mirai detects them and starts analysing right away. Otherwise, add them in
**Preferences → Engines**. The [user guide](docs/user/GUIDE.md#what-you-need-from-katago) covers
recommended setups and first-run behaviour.

Open a record and press <kbd>Space</kbd> for live analysis, or <kbd>Ctrl</kbd>+<kbd>A</kbd>
to analyse the whole game.

mirai follows your desktop language. To try another, start it with `LANGUAGE`, for example
`LANGUAGE=zh_CN mirai`.

## 🌐 Running over a network

On the machine that runs KataGo:

```
mirai-server --generate-token
mirai-server --config server.toml
```

For the token, UDP port, server configuration and certificate verification, see
the [remote-engine guide](docs/user/GUIDE.md#7-using-a-remote-engine).

## 💬 Feedback

mirai is young and moving quickly, and there are no releases yet: build it from this
repository. Bugs, rough edges and ideas are all welcome as
[issues](https://github.com/zhaob1n/mirai/issues). Tell us what you were doing, what you
expected and what happened instead; for an engine problem, the newest file in
`~/.local/share/mirai/katago-logs/` usually says why.

## 📖 Documentation

- Users: [docs/user/GUIDE.md](docs/user/GUIDE.md) — first run, the window, analysis, online
  records, playing, remote engines, settings and keys.
- Translators: [docs/dev/TRANSLATING.md](docs/dev/TRANSLATING.md).
- Contributors and agents: [AGENTS.md](AGENTS.md).

## License

GNU General Public License, version 3 or later ([`LICENSE`](LICENSE)).

mirai is free software: you can redistribute it and/or modify it under the terms of the GNU
General Public License as published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version. It is distributed in the hope that it will be
useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
