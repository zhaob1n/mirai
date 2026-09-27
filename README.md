# mirai

A KataGo analysis and playing GUI for the Linux desktop, plus a purpose-built protocol for
driving KataGo over a network.

![mirai](docs/user/preview.png)

Point it at a local KataGo and it analyses positions, reviews SGF files and plays games
against you. Point it at `mirai-server` on the machine with the GPU and it behaves
identically from a laptop that has none.

---

## What it does

**Analysis.** Live pondering of the position under the cursor. Candidates show win rate, score
lead and visits. Colour is how much the move loses against the engine's pick; visits say how
much to trust that reading. Ownership and policy are one overlay at a time. Hover a candidate
to preview its variation without changing the record. The win-rate graph is always Black's,
with a blunder strip; the sidebar reads the side to move.

**Review and editing.** Open, paste and save SGF, including multi-game collections, keeping
properties mirai does not draw. Navigate the current line and its variations. Editing tools
stay collapsed until you open them. Whole-game analysis fills a blunder list that jumps to
the move.

**Playing.** Play KataGo by visits, time per move, or a human-like profile when the model has
one, on boards from 2×2 to 19×19, with handicap and any of nine rulesets. Byo-yomi, Fischer
or absolute time; clocks sit in the play bar under the board. Two passes open scoring from
KataGo's ownership map, and clicking a group toggles it locally. You can also play both sides
on this device, with no engine.

**Fox.** Browse a Fox Go player's latest public games by exact nickname or UID, then download
and open one for review.

**Remote.** `mirai-server` shares one or more KataGo instances. Clients authenticate with a
token and pin the server certificate on first use. A later certificate that does not match is
refused.

---

## Requirements

- The current stable Rust (edition 2024), which `rust-toolchain.toml` selects. No older compiler is supported.
- GTK 4.22+, libadwaita 1.9+, and Blueprint Compiler 0.22+ with their development packages.
- A KataGo binary and network model (JSON analysis mode, not GTP). mirai does not download KataGo.

On Arch: `pacman -S gtk4 libadwaita blueprint-compiler`. On Debian/Ubuntu, install
`libgtk-4-dev`, `libadwaita-1-dev`, and `blueprint-compiler`.

```
cargo build --release --workspace
```

An Arch PKGBUILD in [`packaging/aur/`](packaging/aur/) builds the GitHub head into
`mirai-git` and `mirai-server-git`; it is not published yet.

```
cd packaging/aur && makepkg -si
```

---

## Getting started

```
cargo run -p mirai
cargo run -p mirai -- game.sgf
```

If KataGo and a model are found, mirai starts automatically; otherwise add them in
Preferences. Open a record and press <kbd>Space</kbd> for live analysis. See the
[user guide](docs/user/GUIDE.md#2-first-run) for setup and first-run behaviour.

---

## Running over a network

On the machine with the GPU:

```
mirai-server --generate-token
mirai-server --config server.toml
```

For the token, UDP port, server configuration and certificate verification, see
the [remote-engine guide](docs/user/GUIDE.md#7-using-a-remote-engine).

---

## Documentation

- Users: [docs/user/GUIDE.md](docs/user/GUIDE.md) — first run, the window, analysis, Fox,
  playing, remote engines, settings and keys.
- Contributors and agents: [AGENTS.md](AGENTS.md).

---

## License

GNU General Public License, version 3 or later ([`LICENSE`](LICENSE)).

mirai is free software: you can redistribute it and/or modify it under the terms of the GNU
General Public License as published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version. It is distributed in the hope that it will be
useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.
