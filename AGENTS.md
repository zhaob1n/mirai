# AGENTS.md

Orientation for anyone — human or agent — picking up `mirai` cold. Read this file, then the
one document your task points at. Do not re-explore the tree; it is mapped for you.

**mirai** is a GTK4/libadwaita desktop application that drives KataGo for Go analysis, review
and play, either as a local subprocess or over a network via a purpose-built protocol
(MRP/1). Five crates, GPL-3.0-or-later.

The code is complete, reviewed and shipping. Treat it as a working system to extend carefully,
not a draft to rewrite.

---

## 1. Where to look

| You need | Read |
|---|---|
| Where does X live? What may I depend on? | [`docs/dev/ARCHITECTURE.md`](docs/dev/ARCHITECTURE.md) — module map, data model, extension recipes |
| Implement or change the network protocol | [`docs/dev/PROTOCOL.md`](docs/dev/PROTOCOL.md) — normative MRP/1 spec, implementable without reading Rust |
| Prove a change works, especially in the GUI | [`docs/dev/TESTING.md`](docs/dev/TESTING.md) — test map, engine verification, GUI harness recipes, debugging playbook |
| Why is it built this way? What already went wrong? | [`docs/archive/RETROSPECTIVE.md`](docs/archive/RETROSPECTIVE.md) — decisions, obstacles, defects found |
| What was originally specified, before any code | [`docs/archive/PLAN.md`](docs/archive/PLAN.md) — historical; the code, not the plan, is authoritative |
| What does the application do, from a user's seat | [`docs/user/GUIDE.md`](docs/user/GUIDE.md) |
| Project front page, install, config | [`README.md`](README.md) |

```
crates/mirai-core     geometry, rules, scoring, game tree, SGF     no I/O, no GUI
crates/mirai-proto    MRP/1 types, frame codec, QUIC transport     knows nothing about KataGo
crates/mirai-engine   Engine trait, LocalEngine, RemoteEngine      knows nothing about GTK
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
`tests/wire_size.rs` and bumping the protocol version.

**INV-7 — one source of truth, per window.** `AppState` (`crates/mirai/src/app.rs`) owns
application state. Widgets never talk to each other; they read `AppState` and listen to its
signals. A widget holding a pointer to another widget is a bug. There is one `AppState` per
window and **several windows are normal** — `open` builds one per file. Exactly three things
are process-wide, and anything else you make process-wide is a bug: the `EnginePool`
(`engines.rs`, one KataGo per profile, held weakly), the config file (written through
`Config::save_merged`, never a whole-file overwrite), and the per-window autosave files.

**INV-8 — `Rc<Ui>` discipline.** Long-lived GTK handlers capture `Weak<Ui>` through the
`with_ui` helper. Exactly **one** strong `Rc<Ui>` exists, parked in a `Cell<Option<Rc<Ui>>>`
and taken by the `close-request` handler; that is the single release point. Transient captures
(file-dialog futures, dialog responses, clipboard paste, the score pump) are deliberately
strong and commented as such. Adding a strong capture to a long-lived handler reintroduces the
leak that review already caught once.

**INV-9 — rendering.** Board, win-rate graph and move tree are custom `gtk::Widget` subclasses
drawn with `gsk` in `snapshot()`. No `GtkDrawingArea`, no cairo.

**INV-10 — borrow before emit.** Release every `RefCell` borrow of the game tree *before*
emitting a signal. Handlers reachable from `tree-changed` / `cursor-changed` / `report` will
borrow it again, and a held borrow turns into a panic in the user's hands.

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

### Toolchain

Rust nightly 1.96 (edition 2024, `rust-version = "1.92"`). Let-chains
(`if let Some(x) = a && cond`) are used throughout and are expected. GTK 4.22+ and
libadwaita 1.9+ development packages are required to build `crates/mirai`.

### Testing expectations

Tests defend observable contracts and must fail on a plausible bug. Do not test plumbing,
defaults, or source text.

- Bug fix → reproduce it first, then fix, then confirm the reproduction is dead.
- New observable contract → a test that pins it.
- Refactor with no behaviour change → no new test; the existing suite is the check.
- GUI change → drive it and look at it. Visual confirmation *is* the proof; see the harness
  recipes in [`docs/dev/TESTING.md`](docs/dev/TESTING.md).

---

## 4. Traps this codebase has already fallen into

Each of these cost real debugging time. They are documented so they cost you none.

| Symptom | Cause |
|---|---|
| A `notify::` handler or `bind_property` silently stops firing | `explicit_notify` on a **derive-generated** setter. It is a pspec flag that disables GObject's automatic notify; it belongs only on properties with a hand-written setter that emits the signal itself. |
| An engine vanishes seconds after you select it | A slower engine activation completing later and overwriting the newer one. Guarded by an activation generation counter in `AppState::activate_profile`; keep the guard. |
| A widget refuses to shrink, or eats the window | `gtk::Paned` resize/shrink flags. A fixed strip wants `resize_end_child(false)` plus a size request, not a hardcoded `position`. |
| A title widget is invisible | `adw::HeaderBar::show_title(false)` hides the *title widget*, not just the text. |
| A screen capture of the running app is black | Wayland. The XWayland root window is not composited. Use the built-in harness, which renders through the app's own GSK renderer. |
| The empty-board win rate is ~35.6%, not ~50% | Correct for the bundled network. Settle such questions by running raw `katago analysis` with the identical query and comparing; do not tune our code toward an expectation. |
| A "did not shut down cleanly" prompt after doing nothing | Guarded now by `tree_has_content`: an autosave with no move, setup stone or comment is neither written nor offered. |

---

## 5. Deliberate simplifications — do not "fix" these by accident

They are commented at the source, and `docs/dev/ARCHITECTURE.md` lists them together.

- **Territory scoring** counts each side's territory *plus the prisoners it holds* (captured
  stones and the opponent's dead stones in its area), which reproduces conventional Japanese
  totals. The mirror formulation is margin-equivalent but displays totals ~3 points low.
- **`Tax::All`** (stone scoring) is approximated with the `Tax::Seki` result and reports
  `approximate = true`; the UI labels it as an estimate.
- **Seki detection** is deliberately narrower than "a group adjacent to a shared dame", which
  would tax ordinary territory. See the comment in `crates/mirai-core/src/score.rs`.
- **`has_button`** is modelled as a flat +0.5 to White; mirai never plays the button.

---

## 6. Scope

Deliberately out of scope: screen-board OCR, joseki dictionaries,
KataGo auto-download, theme skinning, dual-engine comparison. Proposals to add them should be
weighed against the maintenance surface, not accepted by default.

**In scope, decided:** mirai owns the KataGo analysis config. It writes the file itself from
one set of static defaults (`mirai-engine/src/tuning.rs`), editable in Preferences; a
user-supplied `analysis.cfg` stays available and then owns every setting but the two thread
counts. A **measured** calibration — timing a few thread combinations against the real model
and keeping the winner — is wanted as well, as an explicit opt-in per profile, never as
something that runs on its own. It replaces the earlier blanket ban on benchmark wizards,
which ruled out the only honest way to fit unknown hardware: GPU tier cannot be established
from the outside, since only the CUDA and TensorRT backends report device memory and a
model-name table would rot.
