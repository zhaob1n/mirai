# Building mirai: process, obstacles, and how the GUI was actually tested

This is a retrospective of building `mirai` — a Rust/GTK4/libadwaita KataGo GUI plus a
purpose-built remote-analysis protocol — from an empty directory to a working application.
It is written for the next person who has to touch this code, and it is deliberately honest
about what went wrong.

Final shape: 5 crates, ~17.9k lines of Rust, 139 tests, `clippy -D warnings` clean.

| crate | lines | what it is |
|---|---:|---|
| `mirai-core` | 3712 | points, board, rules, scoring, game tree, SGF, clock |
| `mirai-proto` | 1429 | MRP/1 wire types, frame codec, QUIC transport |
| `mirai-engine` | 2579 | `Engine` trait, `LocalEngine`, `RemoteEngine` |
| `mirai-server` | 1132 | headless remote engine host |
| `mirai` | 9068 | the GTK application |

---

## 1. The shape of the work

The plan fixed eight design decisions up front (D1–D8) and thirteen implementation steps.
The decisions that paid off most were the ones that *removed* work:

- **D1 — one engine interface, the KataGo JSON analysis engine, never GTP.** Analysis queries
  are stateless: each one carries `moves`, `initialStones`, `rules`, `komi`. There is no
  engine-side state to keep in sync, so there is no replay/undo machinery, and the remote
  protocol is a pure request/stream forwarder rather than a state mirror. The reference
  implementation this replaces (LizzieYzy) spends 12k lines on exactly that problem.
- **D2 — use KataGo's own point encoding** (`index = y * width + x`, `y = 0` is the top row).
  This makes the ownership and policy arrays byte-identical in ordering to our board array,
  so overlays need zero index remapping. This one decision is why the ownership heat map
  worked on the first try; the classic failure mode here is a transposed map, and there was
  simply no code in which to transpose it.
- **D5 — `tokio::sync::watch` for analysis subscriptions.** A `watch` channel collapses
  superseded values automatically. At a 10 Hz report rate with a GUI that may be busy
  laying out a `ColumnView`, the consumer *should* skip stale reports. Using `watch` meant
  never writing a coalescing queue, and never having a backlog of obsolete reports.

### Execution strategy

I wrote the shared contract surface myself — `point.rs`, `rules.rs`, `mirai-proto/types.rs`,
the `Engine` trait, `frame.rs`, `transport.rs`, `AppState`, `config.rs` — and delegated the
implementations behind those interfaces to parallel subagents. The rule I followed: *own the
decomposition and every cross-slice contract; delegate the filling-in*.

Concretely, before any subagent started I created **compiling stubs with the real public
signatures** for every file that would be delegated. That is more up-front work than writing
a prose spec, but it buys three things:

1. Each agent can run `cargo check -p <crate>` immediately instead of waiting for siblings.
2. Disagreements become compile errors instead of runtime surprises.
3. The integration file (`window.rs`) can be written against APIs that do not exist yet.

Waves: workspace skeleton → (`mirai-core`, `mirai-engine`, `mirai-server`, `RemoteEngine`) →
(remaining core: tree/SGF/score, probe example) → six-way GUI fan-out → clippy sweep → review.

---

## 2. Obstacles, and what fixed them

### 2.1 Two rustls crypto providers, one panic waiting to happen

`quinn` 0.11 pulls `rustls` with the `ring` provider; `rustls` 0.23's own default features
select `aws-lc-rs`. With both compiled in, `rustls::ClientConfig::builder()` panics at
runtime because there is no unambiguous process-default provider — and it panics *at connect
time*, not at startup, so it would have looked like a networking bug.

Caught before writing a line of transport code by running `cargo tree -i aws-lc-rs` right
after the skeleton compiled. Fix, in the workspace manifest with the reasoning recorded:

```toml
rustls = { version = "0.23.43", default-features = false, features = [
    "ring", "std", "tls12", "logging",
] }
```

Verified by `cargo tree -i aws-lc-rs` reporting no match, and `cargo tree -i ring` showing a
single provider. **Lesson: audit the dependency graph for duplicated "provider" crates before
you build on them.**

### 2.2 Avoiding a dependency for 60 lines of SHA-256

Certificate fingerprints need SHA-256. Adding `sha2` meant either 0.11 (a fresh major with a
changed `Digest` API) or pinning an older line against the plan's version discipline. Since
this is a fingerprint, not a hot path, I wrote SHA-256 into `mirai-proto/src/sha256.rs` and
pinned it with the three standard test vectors (empty, `"abc"`, and a 56-byte input that
forces a second padded block). ~60 lines, zero new supply chain.

### 2.3 The wire-size claim had to be measured, not asserted

The plan claims a live report costs ~2.5 KB instead of ~45 KB of KataGo JSON. That is a
performance claim, so `crates/mirai-proto/tests/wire_size.rs` builds a realistic worst case —
50 candidates, PV length 15, `pv_visits` populated, 361 ownership values — encodes it through
the real frame codec and asserts the framed length. Measured:

```
framed SubMsg::Report = 2622 bytes (50 candidates, PV 15, pv_visits, 361 ownership)
framed ClientMsg::Open with 200 moves = 500 bytes
worst winrate err 7.644e-6, lead err 0.01250, ownership err 0.00394
```

The same test pins the quantisation error budget (winrate ≤ 1e-4, lead ≤ 0.02 pt,
ownership ≤ 0.005), so a future change to a scale factor fails loudly.

### 2.4 A plan expectation that was simply wrong about reality

The plan's acceptance criterion for the engine probe required the empty-board win rate to
land in 45–55%. The bundled network reports **35.6%**. The tempting move is to "fix" our
code until the number matches.

Instead the agent driving that check ran raw `katago analysis` with the identical query and
got `rootInfo.winrate = 0.353740327` — mirai reproduces KataGo exactly. A komi sweep
confirmed the perspective handling: komi 0 → 94.3% for Black, komi 7.5 → 35.4%, komi 30 →
0.1%, monotonically decreasing, which is only possible if the reported winrate really is
Black's. A separate probe with asymmetric initial stones confirmed the ownership index order
(`ownership[0]` = A19 top-left = Black, `ownership[360]` = T1 bottom-right = White).

So the criterion was wrong for this network, not the code. **Lesson: when an acceptance
number fails, first prove what the ground truth is; do not tune toward the expectation.**

### 2.5 Cancellation was 30 seconds late

The plan requires that killing a client makes the server drop the subscription within ~1 s.
Measured: ~30 s — the QUIC idle timeout. Root cause is honest and not a mirai bug: a
process killed with SIGINT never sends a QUIC `CONNECTION_CLOSE`, and UDP has no FIN, so the
server can only notice by timing out.

Two possible fixes: shorten the idle timeout (papering over it, and it would hurt real
users on flaky links), or close gracefully on the client. I took the second: added tokio's
`signal` feature and a `ctrl_c` arm that drops the subscription and the engine, then gives
quinn 200 ms to flush. Measured after the fix:

```
SIGINT  15:19:19.324   (client)
server  12:19:19.3255  connection closed session=3
server  12:19:19.3257  subscription dropped session=3 sub=1
```

Under a millisecond. And critically, I verified the *engine* actually stopped rather than
merely being abandoned, by sampling `/proc/<pid>/stat` utime+stime over a 4-second window:

```
katago pid 6088: 0.0% CPU over 4s while no subscription is open
```

`ps` was useless here — its `%CPU` is a lifetime average and read 11.4% for a completely idle
process. **Lesson: to prove something stopped, measure a rate, not a total.**

---

## 3. Testing the GUI — the interesting part

This was the hardest problem in the project, and the solution is the part I would reuse.

### 3.1 The first attempt failed: screenshots came back black

The obvious approach is to launch the app and grab the screen. On this machine that does not
work. The session is Wayland (`WAYLAND_DISPLAY=wayland-1`) with XWayland for X11 clients:

- ImageMagick's `import` failed with a misleading `missing an image filename` — that build
  has no X11 delegate.
- `ffmpeg -f x11grab -i :0` succeeded and produced a **1920×1080 pure-black PNG**. Under
  Wayland, window contents are composited by the compositor; XWayland's root window has
  nothing drawn into it, so grabbing the root captures nothing.
- No `grim`, `xwd`, `spectacle`, `flameshot`, or `maim` installed.

I also briefly forced `GDK_BACKEND=x11` to make the grab work. That was the wrong instinct
and the user correctly called it out: mirai should run on Wayland by default, and a test that
only passes on a different backend is not testing the shipped configuration. I dropped the
override; there is no `GDK_BACKEND` anywhere in the repo.

### 3.2 The fix: make the app render itself

The renderer that actually draws the window is inside the process. So ask it directly
(`crates/mirai/src/harness.rs`):

```rust
let paintable = gtk::WidgetPaintable::new(Some(&window));
let snapshot = gtk::Snapshot::new();
paintable.snapshot(&snapshot, w as f64, h as f64);
let node = snapshot.to_node().ok_or("nothing was drawn")?;

let renderer = window.native().and_then(|n| n.renderer()).ok_or("no renderer")?;
let texture = renderer.render_texture(&node, None);
texture.save_to_png(path)
```

This is compositor-independent, needs no external tooling, works on the native Wayland
backend, and captures exactly the pixels GSK produces — including the custom `snapshot()`
implementations of `BoardView`, `WinrateGraph` and `MoveTreeView`, which are the parts most
likely to be wrong.

### 3.3 Driving the UI through real code paths

A screenshot is useless without a way to *get* the app into an interesting state. Rather than
synthesising input events, the harness activates the application's own `GAction`s:

```rust
window.activate_action("win.toggle-analysis", None)
```

`WidgetExt::activate_action` resolves the prefix through the widget's action muxer, so this
reaches **exactly the handler a keyboard accelerator would**. There is no parallel test-only
code path — pressing <kbd>space</kbd> and the harness step `action:win.toggle-analysis` run
identical code.

For dialogs, which are not action-driven, the harness has a generic `press:` step that walks
the widget tree from the window root (a presented `adw::Dialog` is a descendant of the
window) looking for a `gtk::Button` whose label matches, and emits `clicked`. Again: generic
driver, no scaffolding inside the dialog code.

The whole thing is a comma-separated script in an environment variable, gated on
`#[cfg(debug_assertions)]` and inert unless set:

```
MIRAI_HARNESS="wait:2500,action:win.next10,action:win.toggle-analysis,wait:10000,
               shot:/tmp/k1.png,action:win.analyse-game,wait:60000,shot:/tmp/k2.png,quit"
```

Steps: `wait:<ms>`, `action:<prefix.name>[=<string arg>]`, `press:<button label>`,
`shot:<path>`, `quit`. The parser has unit tests for every step kind, and for the fact that a
malformed `wait:soon` is dropped rather than silently becoming zero.

One wrinkle: `WidgetPaintable::snapshot` yields nothing if the window has not drawn since the
last change, so two of the first three screenshots came back `nothing was drawn`. The fix is
in the harness rather than in test timing: `queue_draw()` then retry for up to 12 frames.

### 3.4 What this actually caught

Every one of these was found by looking at rendered output, not by a test suite:

| Defect | Symptom in the screenshot | Root cause | Fix |
|---|---|---|---|
| **`explicit_notify` on generated setters** | Header read `No engine` while the engine was demonstrably running and analysing | `explicit_notify` is a *pspec flag* that disables GObject's automatic `notify::` emission. It belongs only on properties with a hand-written setter that emits the signal itself. On a derive-generated setter it silently breaks every `notify::` handler and every `bind_property` target — which also killed the <kbd>c</kbd> and <kbd>n</kbd> display toggles. | Removed the flag from the seven properties with generated setters; kept it on the three with custom setters that call `notify_*()` themselves |
| **Winrate graph ate half the window** | Board squeezed into the top third, ~900 px of empty graph | `gtk::Paned` with a hardcoded `position(640)` and a resizable end child | `resize_end_child(false)` + `size_request(-1, 170)`: the graph keeps its height and all growth goes to the board, while remaining user-draggable |
| **Sidebar pages unreachable** | A stray `✕` where the Analysis/Moves/Comment switcher should be | `adw::HeaderBar::show_title(false)` hides the *title widget*, and the title widget **was** the `ViewSwitcher`. The `✕` was a second set of window controls. | Turn `show_title` back on; disable the duplicate title buttons instead |
| **Engine activation race** | Remote engine connected, then the server logged the connection closing 3 s later with no subscription | `activate_profile` is async and a local KataGo takes ~6 s to load its net. The *older* local activation finished last and installed itself over the newer remote engine, dropping it. | Generation counter: the async completion discards itself if a newer activation has started (`discarding a superseded engine activation profile=local-default`) |

That last one is the kind of bug that unit tests structurally cannot find: it needs two real
engines with different startup latencies, selected in quick succession. It was visible in
thirty seconds of reading a server log next to a screenshot.

### 3.5 A defect the user found before I did

While agents were iterating, the user hit a dialog reading *"mirai did not shut down cleanly.
An autosaved copy of the game record you were looking at is available."* The clean-exit flag
logic was correct — agents were killing the app, which is what the flag is for — but the
autosave on disk was:

```
(;FF[4]GM[1]CA[UTF-8]AP[mirai:0.1.0]SZ[19]KM[7.5]RU[chinese])
```

An empty board. Offering to restore *nothing* is pure noise, and it would hit real users every
time the app was killed rather than quit. Fixed with a `tree_has_content` predicate used on
both sides — never write an autosave without a move, setup stone, or comment, and never offer
to restore one — plus six unit tests including the boundary cases (a pass counts; whitespace
in a comment does not; marks alone do not; content deep in a variation is found).

**Lesson: "technically correct" is not the bar. The prompt was doing what it was designed to
do and was still wrong.**

### 3.6 The verification that mattered most

The plan called out one check as the canary for the whole point-encoding design: turn on the
ownership overlay and confirm the black/white regions match the stones, because *a transpose
is immediately visible as a mirrored map*. Driven through the harness on a real game 30 moves
in, the dark shading sits squarely over Black's bottom-left group. D2 held end to end, from
KataGo's JSON through the quantiser, the wire format, the tree, and into a
`gdk::MemoryTextureBuilder` texture.

---

## 4. Coordination notes

Six GUI agents ran concurrently against frozen stub signatures. What worked:

- **Freeze the API, allow additions.** "You may ADD methods; you MUST NOT change a listed
  signature" prevented every integration break but one.
- **Answer contract questions centrally and make the answer binding.** `WindowShell` asked
  whether `engine_menu_model` returns `gio::Menu` or `gio::MenuModel`, and whether the board
  exposed `set_pv_preview`. I answered with a decision and pushed the same decision to the
  agents that had to implement it, rather than letting them negotiate.
- **Agents self-organised over IRC** for the shared GPU — they queued app launches among
  themselves without being asked, and one correctly warned the others not to kill my server's
  KataGo process.

What went wrong: **repeated network outages killed whole waves of agents mid-flight**, twice.
The first outage lost ~10 minutes of work across four agents; nothing had been written to
disk. The mitigations were (a) instruct agents to write complete files early and often rather
than accumulating work in context, and (b) *resume the existing idle agents by messaging them*
instead of re-dispatching new ones, which preserved their context and cost nothing.

---

## 5. The review round

A dedicated reviewer agent read all five crates end to end. First round: **no blockers**, two
majors and one minor. All three were fixed and re-verified by the same reviewer, which had the
whole workspace in context and could therefore attack its own findings rather than re-derive
them. Second round verdict: **ship** — all three resolved, no new findings, and none of the
properties it had previously cleared disturbed.

**1 — `Rc<Ui>` reference cycle (major, `window.rs`).** Every signal handler captured a strong
`Rc<Ui>` and was itself owned by something `Ui` owns, so `Ui → AppState → handler → Ui`
never broke: the `Ui`, the engine and the tokio handle leaked until process exit.

The fix is more interesting than the diagnosis. The obvious move — weaken every capture —
would have *broken the application*, because `present()` builds `Rc::new(Ui{..})` as a local
and drops it on return. The strong clones inside the handlers were the `Ui`'s only owners;
the leak was also, accidentally, the lifetime mechanism. Weakening all of them would have
freed the `Ui` the instant `present` returned and turned every handler into a silent no-op.

So the design is: every long-lived handler takes `Weak<Ui>` through a `with_ui` helper, and
`connect_close` keeps exactly one strong reference in a `Cell<Option<Rc<Ui>>>` which it
`take()`s on `close-request`. The cycle now has one owner and one release point.

That is a change that unit tests cannot fully validate, so it was verified by running the
real application: load a 30-node SGF, navigate, stream 6.5k visits of live analysis, toggle
the ownership overlay, then trigger the built-in `window.close` action. All handlers still
fired, the teardown ran in order (autosave written, `clean-exit` flag written, engine
dropped), the process exited 0, and no orphaned KataGo was left behind.

**2 — territory scoring showed totals no player would recognise (major, `score.rs`).** The
original formula computed each side's score as *territory minus the prisoners you lost*.
That produces the correct margin, but both absolute totals come out low by the total prisoner
count: `Black 24 — White 39.5` where a Japanese scorer displays `27 — 42.5`. The margin was
right and the result string was right, so no test caught it; it was purely a
"the number on screen is wrong" defect. Now each side gets *territory plus the prisoners you
hold* — captured stones plus the opponent's dead stones in your area. The algebra is
margin-preserving, and the test expectations moved to the conventional values with `W+15.5`
unchanged.

**3 — duplicated position-to-request walk (minor).** `AppState::request_for_cursor` and
`batch.rs::request_for_node` each implemented the node-to-`AnalyzeReq` walk, including the
late-setup fallback. They agreed, but would have drifted. There is now one implementation,
`AppState::request_for_node`; live analysis and whole-game analysis both go through it.

What the review explicitly cleared: no `RefCell` double-borrow is reachable (every tree
borrow is released before signal emission), the cancellation contract holds on both engines,
D2/D3 are correct at every display site, and no panic path is reachable from SGF, KataGo, or
network input.

---

## 6. What I would tell the next maintainer

1. **The harness is the fastest way to see a change.** `MIRAI_HARNESS` + `shot:` gives you a
   PNG of the real renderer in about fifteen seconds. Use it before reasoning about layout.
2. **`explicit_notify` is a trap.** If a `notify::` handler or a `bind_property` stops firing,
   check the pspec flags first.
3. **Every engine-derived value is Black-perspective** (D3). Convert at display time with
   `winrate_for(to_play)` / `score_lead_for(to_play)`, never in storage. A double conversion
   looks plausible and is nearly invisible in a screenshot.
4. **Dropping a `Subscription` is the only cancellation mechanism.** Local sends
   `terminate`, remote sends `Cancel` + `stop_sending`. Do not invent a second one.
5. **The scoring code documents two deliberate simplifications** (territory totals offset by
   the prisoner count with the margin unchanged; a narrowed seki rule). They are recorded in
   the source with the exact two-line change needed if conventional display totals are wanted.
6. **`score.rs`, `board.rs` and `sgf.rs` are the load-bearing correctness code.** The SGF
   unknown-property preservation contract is tested against a real 25 765-byte LizzieYzy file
   rather than a synthetic `FOO[bar]`, because the whole point is surviving other people's
   files.
