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
- GTK 4.22+, libadwaita 1.9+, libsoup 3 and Blueprint Compiler 0.22+, with their development
  packages.
- GNU gettext, for the translations.
- [`just`](https://github.com/casey/just), to install.

|Distribution|Packages|
|---|---|
|Arch|`gtk4 libadwaita libsoup3 blueprint-compiler gettext just`|
|Debian / Ubuntu|`libgtk-4-dev libadwaita-1-dev libsoup-3.0-dev blueprint-compiler gettext just`|
|Fedora|`gtk4-devel libadwaita-devel libsoup3-devel blueprint-compiler gettext just`|

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
