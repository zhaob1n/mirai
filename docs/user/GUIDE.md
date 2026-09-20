# mirai — user guide

A Go board for Linux that talks to KataGo: live analysis, SGF review and editing, and games
against the computer. The engine can run on the same machine or on another one on your network.

[Installing](#1-installing) · [First run](#2-first-run) · [The interface](#3-the-interface) ·
[Analysing](#4-analysing-a-position) · [Reviewing](#5-reviewing-a-game) ·
[Playing](#6-playing) · [Remote engine](#7-using-a-remote-engine) ·
[Settings](#8-settings-reference) · [Keys](#9-keyboard-reference) ·
[Files](#10-files-mirai-writes) · [Troubleshooting](#11-troubleshooting)

---

## 1. Installing

| Need | Version | Package |
|---|---|---|
| GTK | 4.22+ with dev package | `gtk4` / `libgtk-4-dev` |
| libadwaita | 1.9+ with dev package | `libadwaita` / `libadwaita-1-dev` |
| Rust | nightly 1.99+ | only to build, not to run |
| Blueprint Compiler | 0.22+ | only to build the GTK templates |

```
cargo build --release --workspace     # builds mirai and mirai-server
cargo run -p mirai                    # or run it in place
cargo run -p mirai -- game.sgf        # opening a file straight away
```

`mirai` is the board. `mirai-server` is optional and only needed to put the engine on another
machine ([section 7](#7-using-a-remote-engine)).

### What you need from KataGo

mirai never bundles or downloads KataGo. You supply two files:

| File | Typical name | Note |
|---|---|---|
| The program | `katago` | any recent version works |
| A neural network | `something.bin.gz` | |

The third thing KataGo needs — an analysis config — mirai writes for itself, into
`~/.local/share/mirai/katago-logs/`, from the numbers in Preferences. The file name carries
those numbers, so profiles tuned differently keep separate files. If
you would rather keep your own, Preferences → Engines → **Analysis config** → *Custom file*
takes any `analysis.cfg`; KataGo ships one in its `configs` directory.

mirai drives KataGo's JSON **analysis** engine, never GTP — a GTP-only bot or wrapper script
will not work. If you already run KataGo under Lizzie, KaTrain or Sabaki, point mirai at the
same binary and network; nothing of yours is copied or modified.

Boards from 2×2 to 19×19, matching a stock KataGo build.

---

## 2. First run

**A KataGo was found.** mirai writes an engine profile called `local-default`, selects it and
starts it. The header bar shows `Starting local-default…` for a few seconds while the network
loads, then the engine's name and version. On an OpenCL build the *very first* start can take
minutes — see [Troubleshooting](#the-very-first-start-takes-minutes).

First-run discovery looks for `katago` on `PATH`. Networks are merged in this directory order:
`$XDG_DATA_HOME/katago/models`, `$XDG_DATA_HOME/mirai/models`, `~/.katago/models`, then
`katago/models` and `mirai/models` under each `$XDG_DATA_DIRS` entry from left to right (using
the standard defaults when the variable is unset). Within a directory, newer `*.bin.gz` files
come first. The first candidate seeds `local-default`; the rest are one click away in every
local-profile editor, on the model row's list button.

**No KataGo was found.** Preferences opens by itself and the Analysis sidebar reads *"No engine
configured"*.

### Adding a local engine

1. **Preferences → Engines → Add Local…**
2. **Name** — anything unique; it labels the engine in the header-bar menu.
3. **KataGo binary** and **Neural network model** — the list button on the model row offers
   every discovered network by file name, with its directory underneath whenever two share a
   name and the whole path in the tooltip; the folder button beside it takes any other file.
   Both paths must exist or **Save Profile** is refused, with the reason in a banner at the top
   of the page.
4. **Analysis config** — leave it at **Managed by mirai**. Choosing *Custom file* reveals the
   config row, which carries the same pair of buttons. Its list merges every Analysis `*.cfg`
   in `katago/cfg/analysis` and `mirai/cfg/analysis` under `$XDG_CONFIG_HOME`, then under each
   `$XDG_CONFIG_DIRS` entry; `analysis.cfg` comes first within its directory.
5. **Search** and **Batching and Memory** — leave every number at **0**, which means *use
   mirai's default*; the [settings reference](#engines) lists them.
6. **Save Profile.**

The first profile added becomes the active one and starts immediately. Later profiles do not
steal the selection; switch with the engine button in the header bar. The pencil button edits
a profile, the bin deletes it — deleting removes it from mirai's settings only, never from
disk.

---

## 3. The interface

```
┌───────────────────────────────────────────────────────────────────────────────────┐
│ [open][Fox↓][▶] [engine ▾]      Untitled •       [clocks] [New Game…] [⋮] [☰] │ header bar
│                                  local-default                                  │
├──────────────────────────────────────────────┬────────────────────────────────────┤
│ ⓘ Analysing 42/128…                [Cancel]  │   Analysis │ Moves │ Comment       │ sidebar
│──────────────────────────────────────────────│────────────────────────────────────│
│                                              │ B 54.2%   B+1.8                    │
│              ┌─────────────────┐             │ 12k visits · 1.4k/s · ±6.4 points  │
│              │                 │             │ · Black to play                    │
│              │   the  board    │             │────────────────────────────────────│
│              │                 │             │ # Move  Win Score Loss Visits Prior│
│              │                 │             │ 1 Q16  54.2  +1.8 0.00   8.1k   31%│
│              └─────────────────┘             │ 2 D4   53.9  +1.6 0.03   2.4k   22%│
│                                              │ …                                  │
│──────────────────────────────────────────────│ ▾ Blunders (4)                     │
│  ╭──────────────────────────────────────╮    │  ⚫ 38 · −14.2% · played R7        │
│  │  win-rate + score-lead curves        │    │  ⚪ 57 · −8.1% · played C11        │
│  │  ▁▁▂▃▅▅▄▆▇▇   ← blunder strip below  │    │                                    │
│  ╰──────────────────────────────────────╯    │                                    │
├──────────────────────────────────────────────┴────────────────────────────────────┤
│ ⏮ ◀ ▶ ⏭  ▲▼ [Undo][Pass][Resign] ├──────────●──────────┤ B 54.2% +1.8 12k 1.4k/s│ bottom bar
└───────────────────────────────────────────────────────────────────────────────────┘
```

| Region | What it shows |
|---|---|
| **Header bar** | On the left, the ways to get a record — **Open** (its ▾ also offers *Paste SGF* and *Clear Board*), an icon-only **Download from Fox** button, and the icon-only **New Game** button — then the engine button (name + KataGo version; click for profiles and *Preferences*) and the ▶/■ live-analysis toggle. In the middle, the record's name with `•` while unsaved and the engine or current status beneath it. A saved record is named by its file; one that has never been saved — downloaded, pasted or just played — is named `Black vs White` from the record itself, falling back to the event and then to `Untitled`. On the right, the sidebar toggle and Main Menu (☰) |
| **Board** | wood, grid, star points, optional coordinates, stones. The last move is marked with a red dot, or — when move numbers are on — by its number in red. SGF marks (triangle, square, circle, cross, text labels) are drawn |
| **Editor toolbar** | Above the board in review: undo/redo, Play (two stones and an arrow), black/white setup tools, marks, a mark eraser and the three-dot **Board Menu**. The larger foreground stone in Play shows the side to move. Compact groups wrap on narrow windows. Hidden during an active game |
| **Win-rate graph** | solid curve = Black's win rate (left axis 0/50/100); dashed curve = score lead (right axis, never tighter than ±5); vertical line = where you are; coloured bars along the bottom = the blunder strip |
| **Sidebar** | Three pages — **Analysis** (readout, candidate list, blunder list), **Moves** (the branch graph), **Comment** (the current move's comment). The sidebar button in the header bar (<kbd>F9</kbd>, or ☰ → *View* → *Sidebar*) hides it at any window size, giving the board the whole width; at narrow widths it closes by itself and the same button reopens it as an overlay |
| **Bottom bar** | First / previous / next / last, previous / next variation, then — during a timed game — both clocks, the one counting shown in the accent colour and turning red under ten seconds, followed by the contextual **Undo**, **Pass** and **Resign** controls, a slider along the current line, and a readout of side to move, win rate, score lead, visits and — while a search is running — its speed in visits per second |

The ☰ menu holds *Clear Board*, *Save*, *Save As…*, *Copy SGF*, *Analyse Game*, *Estimate Score*, a *View*
submenu — *Sidebar*, *Coordinates*, *Move Numbers*, *Ownership Overlay*, *Policy Overlay*, each
ticked when it is on — and *Preferences*, *Keyboard Shortcuts*, *About mirai*.

### Mouse on the board

| Action | Effect |
|---|---|
| Left-click (Play tool) | play a real move for the side to play — captures, legality, move number |
| Right-click (Play tool) | take back the current node: delete it and its continuation from **Moves**, then return to its parent. Undo restores the deleted branch; the root cannot be deleted |
| Left-click / right-click (black or white setup tool) | use the selected colour / the opposite colour: place on an empty point, replace an opposite stone, or remove a matching stone. No captures or move numbers; marks are preserved |
| Left-click with a mark tool | apply that tool; right-click does nothing |
| Left-click during scoring | toggle that group alive/dead |
| Shift+right-click, or **Board Menu** | *Play here*, *Set as main line*, *Delete branch*, *Copy SGF*, *Black to Play*, *White to Play*. During a game, record-changing items are disabled |
| Scroll wheel | browse back / forward one move without deleting anything |
| Hover a candidate blob | preview its variation. Non-Play tools clear this preview |

Drag the divider between the board and the graph to make the graph taller.

---

## 4. Analysing a position

<kbd>Space</kbd> (or ▶) starts the engine on the position under the cursor. Moving the cursor
restarts the search on the new position; the old one is abandoned at once, not left running.
The search runs up to **Maximum visits** and the display refreshes every **Report interval**.

### Reading a candidate blob

```
   ╭───────╮
   │ 54.2  │  win rate for the side to move, per cent
   │ +1.8  │  score lead for the side to move, in points
   │ 8.1k  │  visits — how much of the search went here
   ╰───────╯
```

The lower two lines are dropped when the board is drawn too small for them. A move with fewer
than ten visits behind it is drawn without numbers: it is still visible, but it does not turn a
crowded board into a wall of readouts nobody should trust. Two moves keep their numbers whatever
their search — the engine's own first choice, and the one the record plays next.

**Colour is how much the move loses; how solid the blob is, is how much search stands behind
that reading.** The engine's own pick is always the coolest blob — **cyan** — and a candidate
walks through mint and green into yellow, orange and red as it gets worse, in the same three warm
colours the win-rate graph marks a blunder with.

"Worse" is the drop in KataGo's own **utility** against the pick — one number in which the
engine has already blended win rate and score the way it weighs them itself, so a move that
is a shade behind on the win rate but two points behind on the board is graded on both at
once. A gap the search treats as noise stays cool; a gap as large as not having looked at the
move is yellow; half a win of utility is red. A candidate that reads *better* than the pick is
simply cool. The win-rate and score columns still show what they always did; the colour is the
one reading that puts them together.

A move the search barely touched — fewer than ten visits — is **grey**, and faint, and carries
no figures. All three say the same thing: the reading is a rumour, so grey means *unknown*
rather than good or bad, and the fainter the disc the less there is behind it. At ten visits a
candidate gets its colour, its numbers and its full strength together. The engine's pick is
never grey: it is the reference every other loss is measured against, and it stays cyan.

The number in the badge beside each row of the candidate list is the engine's rank; the badge's
colour is that move's grade, the same colour its blob wears on the board.

Rank is **not** "most visits", and the badge colour is **not** the rank. KataGo orders its
moves by play-selection value — how much search the move *deserved*, which is its visit count
trimmed back to what the policy prior and the exploration formula would have spent on it — so
a move with 15% of the top move's visits can still be the one it would play, and a lower-ranked
move can be ahead on the win rate, the score *and* the visits and still sit below one the
engine liked from the start. The number is the menu; the colour is what the move loses. That is
why the cyan blob is not always the one with the biggest visit count, why a green badge can sit
under a yellow one, and why the list is in the engine's order rather than sorted by any
single column.

**The white outline marks the move the record plays next.** Standing on move 57, the outlined
blob is move 58 — so *"did the game play the engine's move?"* is one glance: white on the blue
blob means yes. If the record's move is not among the candidates at all — the search never went
there, or it falls past **Suggestions shown** — the outline appears on a dim empty disc instead,
which is itself the answer. At the end of the record there is nothing to mark and no outline
appears.

**Suggestions shown** controls how many blobs and list rows appear (10 by default). Choose
**All** to keep every move the engine searched; the fading keeps the board readable.

### The candidate list

| Column | Meaning |
|---|---|
| **#** | the engine's own rank; the badge's colour is what the move loses, as on the board |
| **Move** | the point, in standard coordinates |
| **Win** | win rate for the side to move, per cent |
| **Score** | signed score lead for the side to move, in points |
| **Visits** | playouts spent on this move |
| **Loss** | what the move gives away against the engine's pick, in KataGo's own utility — the number the colour is made of. `0.00` for the pick; `—` for a record saved before mirai kept it |
| **Prior** | what the raw network thought of it *before* searching |

**Click a heading to sort by that column**, and again to reverse it. The list opens in the
engine's order and **#** puts it back. Sorting changes only the list: the badge numbers, the
blobs on the board and the move the engine would actually play stay on KataGo's own ranking.
So sorting by **Loss** answers "which of these reads best?" without losing the answer to
"which one would it play?".

Selecting a row previews that move's continuation on the board — which is where a sequence is
worth reading — and double-clicking plays it.

Above it: side to move, win rate, score lead, total visits, `±` the score uncertainty, and,
while the engine is actually searching this position, **how fast it is searching** — `1.4k/s`
is 1400 visits per second. It is measured from the reports themselves, so it reflects what
your machine is really doing right now, and it disappears once the search stops.

High prior with few visits = the network liked it on sight and the search talked itself out of
it. Low prior with many visits = the search found something the network nearly missed.

Columns are resizable; rows are always in the engine's own ranking order. **Click a row** to
pin its variation on the board — the pin survives the next report, so it stays put while the
engine keeps thinking. **Double-click** to play the move.

### Previewing a variation

Hover any candidate blob: the board redraws with that variation played out and numbered from
the current position. Move away and the real position returns. Nothing is written to the game
record.

### Overlays

Both read from the same analysis, so they need a report (live, or one cached on this move).
Switching both on layers policy over ownership and is unreadable — use one at a time.

| Overlay | Key | Reading it |
|---|---|---|
| **Ownership** | <kbd>o</kbd> | shades each point by who is expected to own it at the end — dark for Black, light for White, stronger shade = more certain, unshaded = genuinely unsettled. Groups the engine has written off show as enemy territory while their stones are still on the board |
| **Policy** | <kbd>y</kbd> | shades each point by what the raw network wants to play there, before any search. Comparing it with the searched candidates shows where search disagrees with instinct |

### One-off score estimate

<kbd>Ctrl</kbd>+<kbd>E</kbd> runs a short 400-visit search, derives the dead stones from the
ownership map, counts the board under the game's own rules and komi, and shows the count next
to KataGo's own score lead. Works whether or not live analysis is on.

---

## 5. Reviewing a game

**Opening.** <kbd>Ctrl</kbd>+<kbd>O</kbd>, the Open button, or a file on the command line. If
the file holds several games a dialog lists them — players, size, moves, result, date — and
you choose. <kbd>Ctrl</kbd>+<kbd>V</kbd> pastes a record from the clipboard,
<kbd>Ctrl</kbd>+<kbd>C</kbd> copies the current one out. Anything mirai does not understand in
an SGF file is kept verbatim and written back, so files from other programs survive a round
trip.

**Downloading from Fox.** Click the download button in the header bar, or <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd>. Enter an exact Fox nickname or
numeric UID, and pick a game. The list shows at most the latest 200 public records because
that is the service's fixed history window; players who hide their records are not bypassed.
A successful search is cached: opening the dialog again restores the last query and its
list, so downloading one game does not force another lookup. Click a row — or select it and press **Open Game** — to download and load it. Fox's SGF dialect — including
quarter-point Chinese komi, commentary branches and handicap stones written as opening nodes —
is normalised on import. The result has no local backing file: it is named after its players,
`柯洁 vs 申真谞 •`, and **Save** therefore asks where to store it.

**Several records at once.** Opening a file while mirai is running — from your file manager, or
another `mirai game.sgf` on the command line — gives that record its own window rather than
replacing what you are looking at; passing several files at once opens one window each. The
windows are independent, but they share one KataGo: a second window costs no extra GPU memory
and no second startup wait, and the engine shuts down when the last window using it closes.

**Navigating.** Beyond the [keys](#9-keyboard-reference): the scroll wheel on the board, the
slider in the bottom bar, a click on the graph (hold and drag to scrub), and a click on any
node in the Moves page.

**The curves.** The win-rate curve is always Black's, so a rising curve means Black is doing
better whoever just moved. Curves are drawn only from analysis actually stored on the moves —
unanalysed stretches leave gaps. A whole-game analysis fills them in.

**The blunder strip.** One bar per move, measured from the point of view of whoever played it,
so you never have to work out whose turn it was. A bar is only drawn when both that move and
the position before it have been analysed — an unanalysed stretch is a gap, not a mistake.

| Win rate lost | Bar |
|---|---|
| up to 2 % | nothing |
| 2 – 5 % | yellow |
| 5 – 10 % | orange |
| over 10 % | red |

**Whole-game analysis.** <kbd>Ctrl</kbd>+<kbd>A</kbd> sweeps the main line at **Visits per
move** (100 by default), several positions at a time, with a progress banner and a **Cancel**
button over the board. The curves fill in as each position lands. Cancelling keeps everything
analysed so far. A **Blunders** list appears at the bottom of the Analysis page from the
analyses stored on the main line — it updates as the sweep proceeds, stays when you navigate
or edit a comment, and follows the record if the tree changes:

```
▾ Blunders (4)
   ⚫ 38 · −14.2% · played R7 · best D18
   ⚪ 57 · −8.1% · played C11 · best Q3
```

One line each: the stone the mover played, the move number, the win rate it lost, the move
played, the engine's move. **Click a row to jump there.** Needs a running engine.

**Variations and the move tree.** Playing anywhere other than the end of the line creates a
variation; the original continuation is untouched. The Moves page draws every node: filled
dark = Black, filled light = White, hollow = the start or a setup position, a bar through the
disc = a pass, and the current node wears a coloured ring. The main line runs straight down
the panel, each variation branching off into its own column to the right. Click any node to
go there.

| Operation | How |
|---|---|
| Promote a variation to the main line | Shift+right-click or **Board Menu** → *Set as main line* |
| Delete this move and everything after it | Right-click in Play mode, <kbd>Delete</kbd>, or the same menu → *Delete branch* |
| Undo / redo the last edit | <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> |

The start of the game cannot be deleted. Setup on a node that already has a move or a
continuation adds a new variation; further setup clicks stay on that new leaf.

**Stone tools.** The two-stone arrow icon selects Play and shows the current player with
its larger foreground stone. Clicking the black or white stone immediately enters Setup
for that colour: left-click uses it, right-click uses the opposite colour. A matching stone
is removed; an opposite stone is replaced. Neither changes whose turn it is. Use
**Board Menu** → **Black to Play** / **White to Play** to edit the actual player.

**Comments and marks.** The **Comment** page edits the comment on the current move; it is
stored when you navigate away, click out of the box, or save — there is no Apply, and the
whole typing session is one undo. Marks are drawn from the SGF and can be added: triangle,
square, circle, cross, or a text label. A second click of the same shape removes it; a
different mark at that point replaces it. The **A** icon edits a text label; empty label
text deletes it. The eraser icon removes only marks and labels, leaving the stone alone.

---

## 6. Playing

<kbd>Ctrl</kbd>+<kbd>N</kbd> or the **New Game** icon button. To wipe the current record back to
an empty board without starting a game against the engine, use ☰ → *Clear Board* or
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd>. Size, rules and komi stay; stones, comments
and the file path do not.


### The New Game dialog

| Group | Field | Choices / default | Notes |
|---|---|---|---|
| Board | **Size** | 9×9, 13×13, **19×19**, Custom | *Custom* reveals a box accepting 2–19 |
| | **Handicap** | **None**, 2–9 stones | places the standard points as Black; boards too small for the pattern get none |
| | **Komi** | −150 … 150 by halves | follows the ruleset, but overridable; choosing a handicap sets it to 0.5 |
| | **Rules** | nine rulesets, see below | |
| Players | **You play** | **Black**, White, Both (no engine) | *Both* is a two-sided board with no opponent |
| Time control | **Type** | **None**, Absolute, Byo-yomi, Fischer increment | |
| | **Main time (minutes)** | 20 | any type but None |
| | **Byo-yomi periods** | 5 (1–25) | byo-yomi only |
| | **Seconds per period** | 30 (1–600) | byo-yomi only |
| | **Increment (seconds)** | 10 (0–600) | Fischer only |
| Engine strength | **Mode** | **Visits**, Time per move, Human-like | |
| | **Visits per move** | 800 | |
| | **Seconds per move** | 5.0 | |
| | **Human-like profile** | `rank_5k` | |

| Ruleset | Default komi | | Ruleset | Default komi |
|---|---|---|---|---|
| Tromp-Taylor | 7.5 | | Stone scoring | 7.5 |
| Chinese | 7.5 | | AGA | 7.5 |
| Chinese (OGS) | 7.5 | | AGA (button) | 7.0 |
| Japanese | 6.5 | | New Zealand | 7.0 |
| Korean | 6.5 | | | |

The rules are transcribed from KataGo's own definitions, so what the board allows is exactly
what the engine believes is legal.

**What the strength modes mean.** *Visits* is the honest dial: a few hundred visits is a
strong club player, a few thousand is beyond almost everyone. *Time per move* gives a fixed
number of seconds instead, which keeps games moving on a slow machine. *Human-like* asks the
network to imitate a rank rather than to play well, producing human-shaped mistakes instead of
a perfect game that simply stops trying — **it is greyed out unless the loaded network carries
a human-imitation model**, which most do not. A running clock overrides all of this: the
engine will not spend more time on a move than it can afford.

The ruleset and the strength setting are remembered as next time's defaults.

### While the game runs

Click an empty point to move. The board is read-only while the engine thinks (`Thinking… 3.4k
visits` in the status line) and after the game ends. During a game the editor toolbar is
hidden, ordinary right-click is ignored, redo is disabled, and <kbd>Ctrl</kbd>+<kbd>Z</kbd>
takes back the whole exchange rather than a document edit.

**Clocks** appear in the header bar, Black left, White right, with the side to move
highlighted.

| Type | Behaviour |
|---|---|
| Absolute | main time counts down; zero loses on time |
| Fischer | the increment is added each time you complete a move |
| Byo-yomi | main time first; when it runs out you enter your first period and the clock resets to the period length. The bracketed number is how many periods remain, counting the one you are in. Completing a move inside a period resets it in full. Running a period out with none left loses on time |

Starting a byo-yomi game with zero main time drops you straight into the first period.

| | |
|---|---|
| **Pass** | <kbd>p</kbd>. Two passes in a row end the game and open scoring |
| **Undo** | <kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back the whole exchange — the engine's move and yours — cancels any search in progress, and restores both clocks exactly |
| **Resigning** | main menu → **Resign**. No shortcut, deliberately — it is not a key you want to hit by accident. The *engine* resigns on its own when its win rate has stayed below **Resign threshold** for **Resign streak** consecutive moves *and* the game is past the opening — both conditions, so it never gives up on move 3 |

Live analysis works during a game and will show you the engine's own thinking. Turn it off for
a fair game.

### Scoring

After two passes mirai runs a short search, decides the dead stones from the ownership map,
counts under the game's rules and komi, and shows the result:

```
Both players passed.

Black wins by 6.5

Black 45.0 — White 38.5
```

The board switches to a scoring view: dead stones faded, territory marked with small squares
in the owner's colour, and the status line reading *"… — click a group to mark it dead"*.

**Click any group to toggle it alive or dead.** The count updates instantly and entirely
locally — no engine query, no waiting. This is how you fix a misjudged group or settle a seki
by hand. The result dialog: **Close** keeps counting; **Review Game** stops the session so
the record can be edited again; **Analyse game** stops then starts whole-game analysis.

A resignation or a lost flag settles the result on its own; mirai still counts the board and
shows what the count would have been.

---

## 7. Using a remote engine

For a desktop with a GPU and a laptop without one. `mirai-server` holds KataGo open on the
desktop and lends it to the laptop over your network. Nothing leaves your network.

### On the desktop

**1. Make a token** — a long secret the laptop presents to prove it is allowed in.

```
mirai-server --generate-token
```

**2. Write `~/.config/mirai/server.toml`:**

```toml
listen = "0.0.0.0:9678"       # every interface; 127.0.0.1 would be local-only

cert = "cert.pem"             # created on first start if missing
key  = "key.pem"

[[engine]]
name   = "default"
katago = "/path/to/katago"
model  = "/path/to/model.bin.gz"
# config = "/path/to/analysis.cfg"   # optional; left out, the server writes its own
analysis_threads  = 4
search_threads    = 16
nn_max_batch_size = 64

[[token]]
value    = "…paste the 64 characters here…"
name     = "laptop"
max_subs = 4
```

A fully commented example ships as `server.example.toml`. Relative paths resolve next to the
config file, so the directory can be moved as a unit.

**3. Start it** — `mirai-server --config ~/.config/mirai/server.toml`. It loads every
configured KataGo (a few seconds each), then prints its **certificate fingerprint**, a long
hexadecimal string. Leave that visible; you compare it in a moment.
`mirai-server --print-fingerprint` prints it again at any time.

**4. Open UDP port 9678** through the desktop's firewall.

One server serves several clients from the one KataGo; it does not start a copy per person.

### On the laptop

**Preferences → Engines → Add Remote…**, then:

| Field | Value |
|---|---|
| **Name** | anything, e.g. `desktop` |
| **Server URL** | `mirai://192.168.1.10:9678` — must start with `mirai://` |
| **Token** | the 64 characters, masked as you type |
| **Engine name** | blank unless the desktop hosts several and you want a particular one |

Press **Test connection**. On the first successful connect you are shown:

```
Trust this server?

mirai://192.168.1.10:9678 presented a certificate with SHA-256

a3:7f:2c:…:91

mirai will refuse to connect if it ever changes.
```

**Compare it with what the server printed.** Matching → **Trust**. Not matching → Cancel;
something between the two machines is answering in the desktop's place.

Trusting *pins* that fingerprint: from then on mirai talks only to a server presenting exactly
that certificate. **Save Profile**, then select the profile from the header-bar engine button.
Everything behaves as it does locally. Changing the Server URL later discards the pin, because
a pin belongs to the address it came from, and you are asked to confirm the new one.

### If mirai later refuses to connect

A certificate that does not match the pinned one is a hard refusal, not a warning you can
click through.

| Cause | What to do |
|---|---|
| The server's `cert.pem`/`key.pem` were deleted or regenerated, or the server was reinstalled | expected. Get the new value with `mirai-server --print-fingerprint`, then on the laptop edit the remote profile → **Test Connection** → check → **Trust** → **Save Profile** |
| Nothing changed on the server | do not click through. Something is intercepting the connection; check the network and the address |

---

## 8. Settings reference

Four pages. Every change is written to disk immediately; there is no Apply button.

### Engines

| Setting | Default | Notes |
|---|---|---|
| Engine profiles | one, if a KataGo was found | radio button = active engine; pencil edits, bin deletes |
| *local* Name / KataGo binary / model | — | both paths must exist to save; the model row's list button holds every discovered network, the folder button any other file |
| *local* Analysis config | Managed by mirai | *Custom file* reveals the config row, whose list button holds every discovered Analysis config; all settings except the two thread counts then come from that file |
| *local* Positions in parallel | 0, meaning 4 | `numAnalysisThreads`: positions searched at once. Four keeps a whole-game sweep and a cursor move from queueing behind each other |
| *local* Threads per position | 0, meaning 16 | `numSearchThreadsPerAnalysisThread`: how hard one position is searched. Raise on a many-core CPU, but the returns fall off past 16 |
| *local* GPU batch size | 0, meaning 64 | `nnMaxBatchSize`. Wants to be at least positions × threads. Hidden while a custom config is selected |
| *local* Neural-net cache | 0, meaning 20 | `nnCacheSizePowerOfTwo`: 2^20 cached evaluations, roughly 3 GiB once warm. Hidden while a custom config is selected |
| *local* Automatic tuning | off | **Tune…** measures the selected binary and model, updates the three performance rows, and waits for **Save Profile** before applying them. Managed configs only |
| *remote* Server URL / Token / Engine name | — / — / blank | blank engine name means the server's first engine |
| *remote* Pinned fingerprint | not pinned | read-only; set by **Test Connection** and your confirmation |

With **Managed by mirai**, `0` in any of those four rows means mirai's own default; with
*Custom file* it means *keep what the file says*, and the row subtitles change to say so. The
generated config sets those four values and nothing else — everything else is KataGo's own
default. Three of them are there because KataGo refuses to start without
`numAnalysisThreads`, `numSearchThreadsPerAnalysisThread` and `nnMaxBatchSize`, which is why
there is a file at all rather than a handful of command-line overrides. The cache is the
exception: KataGo's own analysis-engine default is 2^23, meant for a server analysing games
in bulk, and would settle around 24 GiB on a desktop.

Automatic tuning is deliberately manual and per profile: changing a path or opening Preferences
never starts a benchmark. Wait for engine startup to finish, and finish or cancel whole-game
analysis first. Close other mirai windows; opening one while tuning stops the run. The tuner
temporarily stops this window's normal engine so another search or a second copy of the model
cannot skew the result or exhaust GPU memory. It starts KataGo once per candidate, so first-run
OpenCL kernel tuning can make the run take longer than the usual one or two minutes. **Stop
Tuning** cancels the current query and leaves the saved profile unchanged. On success, review the
measured positions, threads and batch values, then press **Save Profile** to persist and activate
them. The neural-net cache is not changed.

### Analysis

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Maximum visits** | 1 000 000 | 1 000 – 10 000 000 | you want a live search to settle on an answer and stop using the GPU |
| **Report interval** | 100 ms | 20 – 1 000 | the display feels busy, or the link to a remote engine is slow |
| **Suggestions shown** | 10 | All / 1 – 50 | you want every searched move, or a cleaner board — this caps blobs and list rows together |
| **Visits per move** | 100 | 100 – 100 000 | reviewing: 100 is quick, 5 000 is thorough |
| **Analyse on Open** | off | on / off | turn on to start a whole-game sweep whenever a record is opened, pasted or downloaded |

The numeric rows accept typing, scrolling and the keyboard's arrow keys; the old `+`/`−`
steppers were impractical for ranges such as one thousand to ten million. Each page ends with a
**Restore … Defaults** button. Its toast offers **Undo**; engine profiles are never reset.

### Play

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Mode** | Fixed visits | visits / time / human-like | |
| **Visits per move** | 800 | 1 – 1 000 000 | you want a weaker or stronger opponent |
| **Seconds per move** | 5.0 | 0.1 – 300 | using Fixed time |
| **Human model profile** | `rank_5d` | free text | the network supports human imitation and you want another rank |
| **Temperature** | 0.00 | 0 – 2 | 0 always plays the best move; 0.2–0.4 varies the opening |
| **Resign threshold** | 0.05 | 0 – 0.5 | 0 makes the engine play every game out |
| **Resign streak** | 3 | 1 – 10 | it gives up too readily |
| **Default ruleset** | Chinese | nine rulesets | |

### Appearance

| Setting | Default | Effect |
|---|---|---|
| **Coordinates** | on | letters and numbers around the board |
| **Move numbers** | off | number every stone; the last move's number is red, so the dot is not needed |
| **Ownership** | off | the ownership heat map |
| **Policy** | off | the raw-network heat map |
| **Save analysis in SGF** | off | writes stored win rates and candidates into the SGF, so the curves survive a save and reload. Larger files; other programs ignore the extra data |

### The settings file

`~/.config/mirai/config.toml` (strictly `$XDG_CONFIG_HOME`). Plain text, editable by hand
while mirai is closed. A missing file is fine; a malformed one makes mirai fall back to a
first-run configuration rather than start with half of one.

---

## 9. Keyboard reference

| Navigation | | Analysis | | Game | | File | |
|---|---|---|---|---|---|---|---|
| <kbd>Home</kbd> | first move | <kbd>Space</kbd> | live analysis on/off | <kbd>Ctrl</kbd>+<kbd>N</kbd> | new game | <kbd>Ctrl</kbd>+<kbd>O</kbd> | open |
| <kbd>End</kbd> | last move | <kbd>Ctrl</kbd>+<kbd>A</kbd> | analyse whole game | <kbd>p</kbd> | pass | <kbd>Ctrl</kbd>+<kbd>S</kbd> | save |
| <kbd>←</kbd> <kbd>→</kbd> | one move | <kbd>Ctrl</kbd>+<kbd>E</kbd> | estimate score | <kbd>Ctrl</kbd>+<kbd>Z</kbd> | last edit | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> | save as |
| <kbd>Page Up/Down</kbd> | ten moves | <kbd>o</kbd> | ownership overlay | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> | redo | <kbd>Ctrl</kbd>+<kbd>C</kbd> | copy record |
| <kbd>↑</kbd> <kbd>↓</kbd> | variations | <kbd>y</kbd> | policy overlay | <kbd>Delete</kbd> | delete branch | <kbd>Ctrl</kbd>+<kbd>V</kbd> | paste record |
| | | <kbd>c</kbd> | coordinates | | | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> | download from Fox |
| | | <kbd>n</kbd> | move numbers | | | <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> | clear board |
| | | <kbd>F9</kbd> | show/hide sidebar | | | | |

<kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back both players' last moves during a game; in review it
undoes the last edit — a placed stone, a mark, a comment commit — not each keystroke.
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> redoes it (disabled during a game). While a
comment or label field has focus, those keys undo typing in that field instead. The same
table is in the application under ☰ → *Keyboard Shortcuts*.

No dedicated accelerator: switching engine profile, *Preferences*, *Keyboard Shortcuts*,
*About mirai*, and **Board Menu**. These remain reachable with standard keyboard focus.

---

## 10. Files mirai writes

| Path | What |
|---|---|
| `~/.config/mirai/config.toml` | settings and engine profiles |
| `~/.local/share/mirai/autosave-*.sgf` | the record each open window is looking at, one file per window |
| `~/.local/share/mirai/fox-last-search.json` | last Fox search query and game list, restored the next time the download dialog opens |
| `~/.local/share/mirai/katago-logs/` | KataGo's own logs, one file per engine start, and the generated `katago-analysis-*.cfg` |
| `~/.config/mirai/server.toml` | `mirai-server`'s settings, on the machine running it |

(`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` are honoured if set.)

Autosave runs every 30 seconds, and only when there is something worth keeping — a move, a
setup stone, a mark, an explicit side to play, or a comment; a blank board is never saved.
Closing a window **deletes** its autosave, so a file still there on the next start is one a
crash left behind, and that is what mirai offers to restore. The autosave is not your file:
restoring it does not make it the target of a plain Save. KataGo's logs accumulate and can be
deleted at any time, as can the generated analysis config — mirai writes it again whenever
its contents would change. The Fox search cache can be deleted; the next lookup writes it
again. Nothing else is written; your own KataGo installation, model and any analysis config
you supplied are never modified.

---

## 11. Troubleshooting

### The engine will not start

A toast naming the profile, and the engine button falling back to "No engine".

| Cause | Fix |
|---|---|
| A path is wrong | Preferences → Engines → pencil. mirai refuses to save a bad path, but a file can be moved afterwards |
| Not the real KataGo | it must be KataGo's analysis engine; a GTP bot or wrapper will not answer |
| KataGo itself is failing | read `~/.local/share/mirai/katago-logs/` — missing GPU drivers and a mismatched model are the usual two |
| Model too new for the binary | use the pair that shipped together |

### The very first start takes minutes

Possibly ending in `katago did not answer within 180s`.

*Cause:* an OpenCL KataGo tunes itself to your GPU the first time it runs on a given board
size. This genuinely takes minutes, once.

*Fix:* wait it out, watching `~/.local/share/mirai/katago-logs/`. If it does time out, run
`katago benchmark -model … -config …` once from a terminal so the tuning completes with no
clock on it; afterwards mirai starts in seconds. Do the same on the server for a remote
engine.

### Analysis does not appear

| Check | |
|---|---|
| Is ▶ pressed in? | <kbd>Space</kbd> toggles live analysis |
| Does the engine button show a name and version? | otherwise there is no engine |
| Is **Suggestions shown** turned down? | Preferences → Analysis |
| Is **Maximum visits** low? | the search finishes at once and then sits still. Correct, not a hang |
| Overlays blank? | they draw nothing until the first report arrives |

The Analysis page says which case you are in: *"No engine"*, or *"Turn on live analysis, or
analyse the whole game"*.

### The remote connection is refused

| Symptom | Fix |
|---|---|
| Never connects | check the address; open UDP 9678; make sure the server listens on `0.0.0.0`, not `127.0.0.1` |
| Connects then rejected | the token must match exactly, or the server has no `[[token]]` block at all — it starts without one and refuses every client |
| `does not match the pinned` | see [If mirai later refuses to connect](#if-mirai-later-refuses-to-connect) |
| The URL is rejected in Preferences | it must begin with `mirai://` |

### "mirai did not shut down cleanly"

A dialog on start offering to restore the last record. The previous session ended without
closing its windows: a crash, a kill, a power cut, or a logout that did not let the window
close, so its autosave was never cleaned up. **Restore** loads the autosaved record — untitled,
so the first Save asks where to put it, and nothing of yours is overwritten. **Discard** deletes
it. If several windows were open, each new window is offered one of them, most recent first.

### An SGF from another program looks wrong

| Symptom | Cause / fix |
|---|---|
| Only one game opened from a multi-game file | you were shown the list and took the first; reopen and choose |
| Curves are empty | no analysis is stored in the file. Run a whole-game analysis, and turn on *Save analysis in SGF* to keep it |
| Some annotations are not drawn | mirai draws triangles, squares, circles, crosses and text labels. Others are kept in the file untouched and reappear when you save |
| Result or komi looks odd | the file's own values are used as written |
| Will not open at all | not SGF, or damaged; the toast names the problem |

Saving a file mirai opened never strips anything it did not understand.

---

Developers: the documentation starts at [the architecture overview](../dev/ARCHITECTURE.md).
mirai is free software under the GNU General Public License, version 3 or later.
