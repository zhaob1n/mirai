# AGENTS.md

Orientation for anyone — human or agent — picking up `mirai` cold. Read this file, then the
one document your task points at. Do not re-explore the tree; it is mapped for you.

**mirai** is a GTK4/libadwaita desktop application that drives KataGo for Go analysis, review
and play, either as a local subprocess or over a network via a purpose-built protocol
(MRP). Six crates, GPL-3.0-or-later.

The code is complete and reviewed, and it is still in development: there are no external
users yet, so compatibility with older builds is not a constraint. Treat it as a working
system to extend carefully, not a draft to rewrite.

---

## Core principles

Do not modify this section without explicit approval.

### Documentation

- [`docs/dev/`](docs/dev/) and this file are for developers and agents.
- [`docs/user/`](docs/user/) and [`README.md`](README.md) are for users.
- [`docs/archive/`](docs/archive/) is legacy. The code is authoritative.
- Keep documentation short. When the project changes, update it: a new
  reader should recover the decisions and the scars from these files, the
  comments, and the Git history.
- Prefer mermaid over ASCII diagrams.

### Development

- Git history follows [upstream Linux kernel conventions](https://www.kernel.org/doc/html/latest/process/submitting-patches.html):
  focused, bisectable commits and a topic branch per independent change.
  Subject line names the change; the body says why. That includes merge
  commits: `--no-ff` only when the branch carries a series worth summarising
  or when it genuinely diverged, and then the merge body *is* that summary.
  A topic branch holding one commit off the current tip fast-forwards — an
  empty `Merge branch 'x'` carries no information and should not exist.
- Follow Linus Torvalds' code taste.
- Launch a reviewer subagent before a branch is merged into main.
- Helper scripts and tools that paid for themselves stay in the tree so
  the next task can reuse them.

---

## 1. Where to look

| You need | Read |
|---|---|
| Where does X live? What may I depend on? | [`docs/dev/ARCHITECTURE.md`](docs/dev/ARCHITECTURE.md) — module map, data model, extension recipes |
| Implement or change the network protocol | [`docs/dev/PROTOCOL.md`](docs/dev/PROTOCOL.md) — normative MRP spec, implementable without reading Rust |
| Prove a change works, especially in the GUI | [`docs/dev/TESTING.md`](docs/dev/TESTING.md) — crate coverage, engine verification, GUI harness, debugging playbook |
| Draw in a widget, or chase a dropped frame | [`docs/dev/RENDERING.md`](docs/dev/RENDERING.md) — why the custom widgets draw with quads, and the measurements behind it |
| Candidate colour, or why the list is not monotonic | [`docs/dev/CANDIDATE_COLOUR.md`](docs/dev/CANDIDATE_COLOUR.md) — KataGo's `order` is play-selection value, not the win-rate column; what that does to the ramp |
| Fox HTTP, or the Fox SGF dialect | [`docs/dev/FOX_KIFU_API_SPEC.md`](docs/dev/FOX_KIFU_API_SPEC.md) |
| Touch anything the HarmonyOS client depends on | [`../mirai-hmos/docs/dev/UPSTREAM.md`](../mirai-hmos/docs/dev/UPSTREAM.md) — optional adjacent checkout, not a path in this repository. Present only if `mirai-hmos` is checked out beside this one; that client's ledger of what it reuses from here |
| Why is it built this way? What already went wrong? | [`docs/archive/RETROSPECTIVE.md`](docs/archive/RETROSPECTIVE.md) — decisions, obstacles, defects found |
| What was originally specified, before any code | [`docs/archive/PLAN.md`](docs/archive/PLAN.md) — historical; the code, not the plan, is authoritative |
| What does the application do, from a user's seat | [`docs/user/GUIDE.md`](docs/user/GUIDE.md) |
| Project front page and install | [`README.md`](README.md) — product, and Requirements (build dependencies and commands) |
| Client settings, including a hand-edited config | [`docs/user/GUIDE.md`](docs/user/GUIDE.md#8-settings-reference) |

Search user-facing questions in `README.md docs/user/` and implementation questions in
`AGENTS.md docs/dev/`; search `docs/archive/` only when tracing history.

```
crates/mirai-core     geometry, rules, scoring, game tree, SGF     no I/O, no GUI
crates/mirai-proto    MRP types, frame codec, QUIC transport       knows nothing about KataGo
crates/mirai-engine   Engine trait, LocalEngine, RemoteEngine      knows nothing about GTK
crates/mirai-client   shared analysis, session, play, Fox          no GTK, no files
crates/mirai-server   headless host sharing KataGo across clients
crates/mirai          the GTK application                          the only crate that links GTK
```

That layering is a rule, not an observation. A `use gtk::` in `mirai-engine`, or a KataGo JSON
key in `mirai-proto`, is a design break — fix the design, not the import.

---

## 2. Non-negotiable invariants

These are load-bearing. Each is cheap to violate by accident and expensive to debug.
`docs/dev/ARCHITECTURE.md` says where each is enforced.

**INV-1 — point encoding.** `Point(u16)`, `index = y * width + x`, **`y = 0` is the TOP row**,
`Point::PASS == u16::MAX`. This is KataGo's own ordering, so `ownership` and `policy` arrays
index identically to the board array. Never introduce a remap. Boards are 2..=19 per side.

**INV-2 — perspective.** KataGo runs with `reportAnalysisWinratesAs=BLACK`. Everything stored
and transmitted is **Black-perspective**. Convert to side-to-move only where you display it,
with `winrate_for(color)` / `score_lead_for(color)`. A double conversion looks plausible on
screen and is nearly invisible — check every new call site.

**INV-3 — cancellation.** Dropping a `Subscription` is the *only* cancellation mechanism.
Local sends KataGo a `terminate`; remote sends `Cancel` and `stop_sending` on that
subscription's stream. Do not add a second mechanism.

**INV-4 — stateless queries.** Every request carries its whole position. There is no
engine-side session state, and adding some would collapse the remote design.

**INV-5 — komi** travels as `komi_x2: i16`; KataGo accepts only integer/half-integer komi.

**INV-6 — quantisation.** Wire floats are fixed-point; the scales live in
`crates/mirai-proto/src/types.rs`. Round-trip error is budgeted and tested: winrate ≤ 1e-4,
score lead ≤ 0.02 points, ownership ≤ 0.005. Changing a scale means updating
`tests/wire_size.rs` (during development the protocol version does not move).

**INV-7 — one source of truth, per window.** `AppState` (`crates/mirai/src/app.rs`) owns
application state. One `Change` dispatcher in `window.rs` pushes projections to widgets; widgets
never talk to siblings or install competing AppState dispatchers. There is one `AppState` per
window and **several windows are normal**. `MiraiApplication` owns only the process-wide Tokio
runtime and shared `EnginePool` (whose engine entries are weak). Config writes use
`Config::save_merged`; autosaves remain per-window.

**INV-8 — window ownership.** `MiraiWindow` owns exactly one plain `Ui` value in its GObject
state. Long-lived handlers capture `glib::WeakRef<MiraiWindow>` and enter through
`MiraiWindow::with_ui`; stateful controllers do the same, while the custom widgets are handed
their window's `AppState` and hold it directly. `close-request`, `dispose` and
`ApplicationImpl::shutdown` all reduce to `MiraiWindow::shutdown`, whose `take_ui` drops the
`Ui` — and releasing *is* `Ui`'s `Drop`, so a new exit path cannot forget it. Finite async
captures must be explicitly transient and own one teardown path.

**INV-9 — rendering.** Board, win-rate graph and move tree are custom `gtk::Widget` subclasses
drawn with `gsk` in `snapshot()`. No `GtkDrawingArea`, no cairo. Tree-derived projections,
heat-map textures and reusable render nodes are built outside `snapshot()`. Draw with quads —
colour nodes, border nodes, rounded clips, the helpers in `widgets/paint.rs` — and never hand
GSK a `fill` or `stroke` node for a shape they can draw: GSK keys its rasterisation cache on
the path pointer, so a path rebuilt every frame always misses, and rebuilding board-sized paths
cost 30–120 ms a frame. Text is cached per `PangoFont` instead, never per position, so board
text may be deferred while `Layout::cell` moves — never merely because the widget was
reallocated ([`docs/dev/RENDERING.md`](docs/dev/RENDERING.md)).

**INV-10 — borrow and identity discipline.** Release every `RefCell` tree borrow before calling
`changed`, `set_cursor`, `set_report` or `toast`; dispatcher code borrows the tree again.
`NodeId` is arena-local, so anything retained across a tree replacement carries
`NodeRef { epoch, id }` and is validated with `resolve_node`.

---

## 3. Working here

### Commands

```sh
cargo build --workspace --all-targets
cargo test  --workspace                                   # expect 0 failures
cargo clippy --workspace --all-targets -- -D warnings     # expect exit 0
cargo run -p mirai                                        # the application
```

Full detail, including how to drive the GUI headlessly and verify against a real engine, is in
[`docs/dev/TESTING.md`](docs/dev/TESTING.md).

### Rules of engagement

- **Never leave the tree broken.** `clippy -D warnings` and the full test suite pass on every
  commit. If your change needs a lint suppressed, justify it in a comment.
- **Do not add dependencies.** Versions are pinned in the root `[workspace.dependencies]` and
  member crates use `dep.workspace = true`. If you truly need a crate, say so and why rather
  than adding it quietly. Two hard-won constraints: `rustls` is pinned to the `ring` provider
  (`default-features = false`) because a second crypto provider makes
  `ClientConfig::builder()` panic at runtime; SHA-256 is implemented in-tree
  (`mirai-proto/src/sha256.rs`) rather than pulled in for 60 lines.
- **Keep the tree rustfmt- and clippy-clean.** Both are clean across the workspace today
  (`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` exit
  0), so day to day you only need them on what you touched: `cargo fmt`, `cargo clippy -p
  <crate>`. Never hand-format around rustfmt. `clippy --fix` across the tree stays banned: it
  rewrites files you did not read. A new toolchain can add lints to untouched code; fix those
  in their own commit rather than inside a feature change.
- **Every new `.rs` file starts with the SPDX header:**
  ```rust
  // SPDX-License-Identifier: GPL-3.0-or-later
  // Copyright (C) 2026 Huang Zhaobin
  ```
- **Do not commit `target/`.** It is ignored; keep it that way.
- **GTK crates are renamed** in `crates/mirai`: `use gtk::…` is package `gtk4`, `use adw::…` is
  `libadwaita`. `gtk` re-exports `gdk`, `gio`, `glib`, `graphene`, `gsk`, `pango`.
- **mirai runs on Wayland by default.** Never set `GDK_BACKEND` to make something work; a fix
  that only holds under XWayland is not a fix.
- **Follow the [GNOME Human Interface Guidelines](https://developer.gnome.org/hig/)** for every
  user-facing UI change; prefer standard GTK/libadwaita patterns and components.
- **Use [Blueprint](https://jwestman.pages.gitlab.gnome.org/blueprint-compiler/)** for static UI
  hierarchy and layout; keep state, business logic and genuinely dynamic UI in Rust.

### Toolchain

Rust stable, selected by `rust-toolchain.toml`; the minimum is `rust-version` in the root
`Cargo.toml`, currently 1.92 (edition 2024). No nightly feature is used, and none should be
added. Raise `rust-version` whenever a dependency update needs a newer compiler — that is its
only reason to move — and check the new floor with `cargo +<version> check --workspace
--all-targets`. Let-chains (`if let Some(x) = a && cond`) are used throughout and are
expected. GTK 4.22+, libadwaita 1.9+ and Blueprint Compiler 0.22+ are required to build `mirai`.

### Testing expectations

Tests defend observable contracts and must fail on a plausible bug. Do not test plumbing,
defaults, or source text.

- Bug fix → reproduce it first, then fix, then confirm the reproduction is dead.
- New observable contract → a test that pins it.
- Refactor with no behaviour change → no new test; the existing suite is the check.
- GUI change → drive it and look at it. Visual confirmation *is* the proof; see the harness
  recipes in [`docs/dev/TESTING.md`](docs/dev/TESTING.md).
- Per-frame cost → a screenshot cannot prove it and the suite cannot see it. Measure with
  `MIRAI_FRAMES=1` and `tools/perf/`, and quote the numbers
  ([`docs/dev/RENDERING.md`](docs/dev/RENDERING.md)).

---

## 4. Traps this codebase has already fallen into

The playbook — symptom, cause, and where the guard lives — is
[`docs/dev/TESTING.md`](docs/dev/TESTING.md#7-debugging-playbook). Do not keep a second copy here.

---

## 5. Deliberate simplifications — do not "fix" these by accident

The full list, with the source of each, is
[`docs/dev/ARCHITECTURE.md`](docs/dev/ARCHITECTURE.md#10-known-simplifications).
They are commented at the source. Do not "fix" one by accident.

---

## 6. Scope

Deliberately out of scope: screen-board OCR, joseki dictionaries,
KataGo auto-download, theme skinning, dual-engine comparison. Proposals to add them should be
weighed against the maintenance surface, not accepted by default.

**In scope, decided:** mirai owns the KataGo analysis config. It writes the file itself from
one set of static defaults (`mirai-engine/src/tuning.rs`), editable in Preferences; a
user-supplied `analysis.cfg` stays available and then owns every setting but the two thread
counts. A measured calibration — timing a few thread combinations against the real model and
keeping the winner — is an explicit opt-in per profile, never something that runs on its own.
Why a reported device memory or a model-name table is not a substitute is
[`docs/dev/ARCHITECTURE.md`](docs/dev/ARCHITECTURE.md#22-mirai-generates-katagos-analysis-config).
