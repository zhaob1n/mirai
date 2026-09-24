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

Build dependencies, package names and the release build are in the
[README Requirements](../../README.md#requirements). `mirai` is the board. `mirai-server` is
optional and only needed to put the engine on another machine
([section 7](#7-using-a-remote-engine)).

### What you need from KataGo

mirai never bundles or downloads KataGo. You supply two files:

| File | Typical name | Note |
|---|---|---|
| The program | `katago` | any recent version works |
| A neural network | `something.bin.gz` | |

The third thing KataGo needs — an analysis config — mirai writes for itself, into
`~/.local/share/mirai/katago-logs/`, from the numbers in Preferences. The file name carries
those numbers, so profiles tuned differently keep separate files. If you would rather keep
your own, Preferences → Engines → **Analysis config** → *Custom file* takes any
`analysis.cfg`; KataGo ships one in its `configs` directory.

mirai drives KataGo's JSON **analysis** engine, never GTP — a GTP-only bot or wrapper script
will not work. If you already run KataGo under Lizzie, KaTrain or Sabaki, point mirai at the
same binary and network; nothing of yours is copied or modified.

Boards from 2×2 to 19×19, matching a stock KataGo build.

---

## 2. First run

**A KataGo was found.** mirai writes an engine profile called `local-default`, selects it and
starts it. The engine button shows `Starting local-default…` while the network loads, then
the engine's name and version, as `name (version)`. The window subtitle is only the current
status — Thinking, Counting, and so on — and is empty when nothing is happening. The engine
name is not repeated there. On an OpenCL build the *very first* start can take minutes — see
[Troubleshooting](#the-very-first-start-takes-minutes).

First-run discovery looks for `katago` on `PATH`. Networks are merged in this directory order:
`$XDG_DATA_HOME/katago/models`, `$XDG_DATA_HOME/mirai/models`, `~/.katago/models`, then
`katago/models` and `mirai/models` under each `$XDG_DATA_DIRS` entry from left to right (using
the standard defaults when the variable is unset). Within a directory, newer `*.bin.gz` files
come first. The first candidate seeds `local-default`; the rest are one click away in every
local-profile editor, on the model row's list button.

**No KataGo was found.** Preferences does not open by itself. The Analysis page shows **No
Engine Configured**, with the description *Add a local KataGo or a remote mirai-server in
Preferences* and a **Preferences** button. The board and its navigation still work. A record
that already stores analysis shows that cached panel instead of the empty page; it does not
invent a live search speed.

### Adding a local engine

1. **Preferences → Engines → Add Local Engine**
2. **Name** — anything unique; it labels the engine button and its menu.
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
steal the selection; switch with the engine button. The pencil edits a profile, the bin
deletes it — deleting removes it from mirai's settings only, never from disk.

---

## 3. The interface

The window is a review board first. Editing tools are collapsed until you open them. The
engine name appears once, on the engine button. Navigation belongs to the board; it does not
repeat the analysis numbers.

| Region | What it shows |
|---|---|
| **Header bar** | Left: **Download from Fox** is the main half of a split button; its arrow menu offers *Open File…* and *Paste SGF*. **New Game**, the engine button and the live-analysis toggle follow. The engine button shows the profile's name and KataGo version, and shrinks rather than duplicating that name in the subtitle; its tooltip is `Analysis Engine —` plus the full label, and its menu is the profiles plus *Preferences…*. Centre: the record title, with `•` while unsaved, and a status subtitle only when there is one. A saved record is named by its file; one that has never been saved is `Black vs White` from the record, then the event, then `Untitled`. Right: sidebar toggle and Main Menu. *Clear Board* is only in the Main Menu |
| **Board** | wood, grid, star points, optional coordinates, stones. The last move is a red dot, or — when move numbers are on — its number in red. SGF marks (triangle, square, circle, cross, text labels) are drawn |
| **Editor toolbar** | Collapsed by default. Open it with **Editing Tools** on the board navigation bar, or Main Menu → *View* → *Editing Tools*. It holds undo/redo, Play, black and white setup, marks and the mark eraser. The larger foreground stone in Play shows the side to move. An active game forces it closed and disables the toggle; choosing a mark or setup tool opens it. Closing it returns to Play |
| **Win-rate graph** | Always Black's view. Solid curve = Black win rate (left axis 0/50/100); dashed curve = Black score lead (right axis, never tighter than ±5). The cursor reads `Black 54.2%`. Coloured bars along the bottom are the blunder strip. Tooltip: Black win rate and Black score lead over the main line. Drag the divider above it to resize it; <kbd>g</kbd>, or Main Menu → *View* → *Win-Rate Graph*, hides it. Both are remembered |
| **Sidebar** | **Analysis**, **Moves**, **Comment**. Analysis reads the side to move, not Black. <kbd>F9</kbd>, or Main Menu → *View* → *Sidebar*, hides it at any width. At 926 or narrower the sidebar closes and the same button reopens it as an overlay |
| **Board navigation** | Under the board only: first / previous / next / last, previous / next variation, the slider, a position such as `12 / 80 · W`, **Editing Tools**, and **Board Menu**. The position tooltip spells out `Move 12 of 80 · White to play`. There is no win-rate readout here |
| **Play bar** | Only while a game is running. Timed games show both clocks here — `●` Black, `○` White, the side to move in the accent colour — then **Undo**, **Pass**, and, against the engine, **Resign**. During a game the graph, board navigation and sidebar are hidden, and their switches are disabled, so the board takes the window; they return when the game ends, the sidebar as you left it |

The Main Menu holds *Clear Board*, *Save*, *Save As…*, *Copy SGF*, *Analyse Game*, *Estimate
Score*, a *View* submenu — *Sidebar*, *Win-Rate Graph*, *Editing Tools*, *Loss and Prior
Columns*, *Coordinates*, *Move Numbers*, *Ownership Overlay*, *Policy Overlay* — and
*Preferences*, *Keyboard Shortcuts*, *About
mirai*. The two overlay items select the same single overlay as Preferences; they are not two
layers at once.

### Mouse on the board

| Action | Effect |
|---|---|
| Left-click (Play tool) | play a real move for the side to play — captures, legality, move number |
| Right-click (Play tool) | take back the current node: delete it and its continuation, then return to its parent. Undo restores the deleted branch; the root cannot be deleted. This is not a menu |
| Left-click / right-click (black or white setup tool) | use the selected colour / the opposite colour: place on an empty point, replace an opposite stone, or remove a matching stone. No captures or move numbers; marks are preserved |
| Left-click with a mark tool | apply that tool; right-click does nothing |
| Left-click during scoring | toggle that group alive/dead |
| Shift+right-click, or **Board Menu** | *Play Here*, *Set as Main Line*, *Delete Branch*, *Copy SGF*, *Black to Play*, *White to Play*. During a game, record-changing items are disabled. **Board Menu** stays on the navigation bar when the editor is collapsed |
| Scroll wheel | browse back / forward one move without deleting anything |
| Hover a candidate blob | preview its variation. Non-Play tools clear this preview |
| Hover an intersection (Play or setup tool) | show a translucent stone where a left-click would place one: the side to play on a legal point, or the setup colour on an empty point. During a game it appears only on your turn |

Drag the divider between the board and the graph to make the graph taller or shorter. mirai
remembers the height, and opens new windows at a size where the board fills its area.
Click the graph, or drag along it, to move along the main line.

---

## 4. Analysing a position

<kbd>Space</kbd> (or the header toggle) starts the engine on the position under the cursor.
Moving the cursor restarts the search on the new position; the old one is abandoned at once.
The search runs up to **Maximum Visits** and the display refreshes every **Report Interval**.
Changing either while a search is running restarts it, once the row has stopped moving; the
number itself is saved at once.

The graph reads the position, always from Black's side: the cursor label is Black's win rate.
The sidebar lists the moves for the side to move, from that side's point of view; a small
black or white stone at its top says which side that is. A stored analysis on the current
node is shown even with no engine configured; visits per second appear only while a live
search is producing them.

### Reading a candidate blob

```
   ╭───────╮
   │ 54.2  │  win rate for the side to move, per cent
   │ +1.8  │  score lead for the side to move, in points
   │ 8.1k  │  visits — how much of the search went here
   ╰───────╯
```

The lower two lines are dropped when the board is drawn too small for them. A move with fewer
than ten visits is drawn without numbers. Two moves keep their numbers whatever their search
— the engine's own first choice, and the one the record plays next.

**Colour is how much the move loses. How solid the blob is, is how much to trust that
reading.** The engine's pick is cyan. A worse move walks through mint and green into yellow,
orange and red — the same warm colours the blunder strip uses. Loss is the engine's own
combined reading of win rate and score, not the visit count and not the rank. Fewer than ten
visits is grey and faint: unknown, not good or bad. The pick is never grey. Why the list
order can disagree with Win, Score and Visits is measured in
[Candidate order versus candidate colour](../dev/CANDIDATE_COLOUR.md); that note is the
derivation, not a second set of on-screen rules.

The number beside each row is the engine's rank. Its colour is the same grade as the blob.
Rank is not "most visits", and the colour is not the rank.

**The white outline marks the move the record plays next.** Standing on move 57, the outlined
blob is move 58. If that move is not among the candidates — the search never went there, or
it falls past **Suggestions Shown** — the outline sits on a dim empty disc. At the end of the
record there is no outline.

**Suggestions Shown** controls how many blobs and list rows appear (10 by default). **All**
keeps every move the engine searched. The engine only sends that many, so raising it
restarts the live search; lowering it does not.

### The candidate list

By default the list is five columns:

| Column | Meaning |
|---|---|
| **#** | the engine's own rank; the number's colour is the same loss grade as the blob |
| **Move** | the point, in standard coordinates |
| **Win** | win rate for the side to move, per cent |
| **Score** | signed score lead for the side to move, in points |
| **Visits** | playouts spent on this move |

**Loss** and **Prior** are off until you ask for them: Main Menu → *View* → *Loss and Prior
Columns*, or right-click any column heading. The order is
then #, Move, Win, Score, Loss, Visits, Prior. Loss is what the move gives away against the
pick (`0.00` for the pick; `—` when a saved record did not keep it). Prior is what the raw
network thought before searching. The extra columns may need horizontal scrolling. Hiding
them while the list is sorted by Loss or Prior returns the sort to #.

**Click a heading to sort by that column**, and again to reverse it. The list opens in the
engine's order, and **#** puts it back. Sorting changes only the list: the rank numbers, the
blobs and the move the engine would play stay on that order. Selecting a row pins its
variation on the board; the pin survives the next report. Double-click, or Enter on the list,
plays the move.

Above the list, one line: the stone of the side to move, `{visits} visits`, and while a live
search is running its speed, such as `1.4k/s`. A cached reading has no speed. Hover the line
for KataGo's spread of the final score. The position's own win rate is the graph's cursor
label; the pick's win rate and lead are the first row. High prior with few visits means the
network liked it and the search did not; low prior with many visits means the search found
something the network nearly missed. Those two columns are the detailed view.

### Previewing a variation

Hover any candidate blob: the board redraws with that variation played out and numbered from
the current position. Move away and the real position returns. Nothing is written to the
record.

### Overlays

Both read from the same analysis, so they need a report (live, or one cached on this move).
Preferences → Appearance → Board → **Overlay** is one choice: **None**, **Ownership** or
**Policy**. <kbd>o</kbd>, <kbd>y</kbd> and the View menu select that same choice; turning one
on turns the other off.

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

**Opening.** <kbd>Ctrl</kbd>+<kbd>O</kbd>, *Open File…* in the Fox button's arrow menu,
or a file on the command line. If the file holds several games a dialog lists them —
players, size, moves, result, date — and
you choose. <kbd>Ctrl</kbd>+<kbd>V</kbd> pastes a record from the clipboard,
<kbd>Ctrl</kbd>+<kbd>C</kbd> copies the current one out. Anything mirai does not understand in
an SGF file is kept verbatim and written back, so files from other programs survive a round
trip.

**Downloading from Fox.** Click the main **Download from Fox** half of the split button,
or press <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd>.
Enter an exact Fox nickname or numeric UID. The list shows at most the latest 200 public
records, which is the service's fixed history window; players who hide their records are not
bypassed. A successful search is cached: opening the dialog again restores the last query and
its list. A click only selects a row. Double-click, Enter on the list, or **Open Game**
downloads and loads that row. Enter in the search box searches; it does not open a game.
Fox's dialect — quarter-point Chinese komi, and handicap stones written as a run of opening
nodes — is normalised on import. The result has no local backing file: it is named after its
players, `柯洁 vs 申真谞 •`, and **Save** therefore asks where to store it.

**Several records at once.** Opening a file while mirai is running — from your file manager,
or another `mirai game.sgf` on the command line — gives that record its own window rather
than replacing what you are looking at; passing several files at once opens one window each.
The windows are independent, but they share one KataGo: a second window costs no extra GPU
memory and no second startup wait, and the engine shuts down when the last window using it
closes.

**Navigating.** Beyond the [keys](#9-keyboard-reference): the scroll wheel on the board, the
slider under the board, a click on the graph (hold and drag to scrub), and a click on any
node in the Moves page.

**The curves.** The win-rate curve is always Black's, so a rising curve means Black is doing
better whoever just moved. The cursor label says `Black`. Curves are drawn only from analysis
actually stored on the moves — unanalysed stretches leave gaps. A whole-game analysis fills
them in.

**The blunder strip.** One bar per move, from the point of view of whoever played it. A bar
is drawn only when both that move and the position before it have been analysed — an
unanalysed stretch is a gap, not a mistake.

| Win rate lost | Bar |
|---|---|
| up to 2 % | nothing |
| 2 – 5 % | yellow |
| 5 – 10 % | orange |
| over 10 % | red |

**Whole-game analysis.** <kbd>Ctrl</kbd>+<kbd>A</kbd> sweeps the main line at **Visits per
Move** (100 by default), several positions at a time, with a progress banner and a **Cancel**
button over the board. The curves fill in as each position lands. Cancelling keeps everything
analysed so far. A **Blunders** list appears at the bottom of the Analysis page from the
analyses stored on the main line. It updates as the sweep proceeds, stays when you navigate
or edit a comment, and follows the record if the tree changes. Each row is one line, and the
title takes the severity colour:

```
▾ Blunders (4)
   ⚫ 38 · −14.2% · played R7 · best D18
   ⚪ 57 · −8.1% · played C11 · best Q3
```

The stone is the mover, then the move number, the win rate it lost, the move played, and the
engine's move when one was stored. **Click a row to jump there.** Click the **Blunders**
heading to collapse the list without losing its contents. Whole-game analysis needs a running
engine; a record that already has analysis still lists its blunders without one.

**Editing tools.** The toolbar is closed when a window opens. **Editing Tools** on the board
navigation bar, or *View* → *Editing Tools*, reveals undo/redo, Play, setup and marks. Play
shows the side to move as the larger stone. Closing the bar returns to Play and does not add
an undo step. An active game closes the bar and disables the toggle; after the game it stays
closed until you open it again.

**Variations and the move tree.** Playing anywhere other than the end of the line creates a
variation; the original continuation is untouched. The Moves page draws every node: filled
dark = Black, filled light = White, hollow = the start or a setup position, a bar through the
disc = a pass, and the current node wears a coloured ring. The main line runs straight down
the panel, each variation branching off into its own column to the right. Click any node to
go there.

| Operation | How |
|---|---|
| Promote a variation to the main line | Shift+right-click or **Board Menu** → *Set as Main Line* |
| Delete this move and everything after it | Right-click in Play, <kbd>Delete</kbd>, or **Board Menu** → *Delete Branch* |
| Undo / redo the last edit | <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> |

The start of the game cannot be deleted. Setup on a node that already has a move or a
continuation adds a new variation; further setup clicks stay on that new leaf.

**Stone tools.** The two-stone arrow selects Play. Clicking the black or white stone enters
Setup for that colour: left-click uses it, right-click uses the opposite colour. A matching
stone is removed; an opposite stone is replaced. Neither changes whose turn it is. Use
**Board Menu** → **Black to Play** / **White to Play** to edit the player to move.

**Comments and marks.** The **Comment** page edits the comment on the current move; it is
stored when you navigate away, click out of the box, or save — there is no Apply, and the
whole typing session is one undo. Marks are drawn from the SGF and can be added: triangle,
square, circle, cross, or a text label. A second click of the same shape removes it; a
different mark at that point replaces it. The label tool edits a text label; empty label
text deletes it. The eraser removes only marks and labels, leaving the stone alone.

---

## 6. Playing

<kbd>Ctrl</kbd>+<kbd>N</kbd> or the **New Game** button. To wipe the current record back to
an empty board without starting a game against the engine, use Main Menu → *Clear Board* or
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd>. Size, rules and komi stay; stones, comments
and the file path do not.

### The New Game dialog

| Group | Field | Choices / default | Notes |
|---|---|---|---|
| Board | **Size** | 9×9, 13×13, **19×19**, Custom | *Custom* reveals **Custom Size**, accepting 2–19 |
| | **Handicap** | **None**, 2–9 stones | places the standard points as Black; boards too small for the pattern get none |
| | **Komi** | −150 … 150 by halves | follows the ruleset, but overridable; choosing a handicap sets it to 0.5 |
| | **Rules** | nine rulesets, see below | |
| Players | **You Play** | **Black**, White, Both (no engine) | *Both (no engine)* hides **Engine Strength**. The group then reads *Play both sides on this device*. Otherwise it reads *The engine takes the other colour* |
| Time control | **Type** | **None**, Absolute, Byo-yomi, Fischer increment | |
| | **Main Time (Minutes)** | 20 | any type but None |
| | **Byo-yomi Periods** | 5 (1–25) | byo-yomi only |
| | **Seconds per Period** | 30 (1–600) | byo-yomi only |
| | **Increment (Seconds)** | 10 (0–600) | Fischer only |
| Engine strength | **Mode** | **Visits**, Time per move, Human-like | hidden for Both (no engine); the hidden value is kept if you switch back |
| | **Visits per Move** | 800 | |
| | **Seconds per Move** | 5.0 (0.1–600) | |
| | **Human Model Profile** | `rank_5k` | unavailable unless the loaded network has a human-imitation model |

**Cancel** closes the dialog without starting. **Start Game** is the default button.

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
network to imitate a rank rather than to play well — it is unavailable unless the loaded
network carries a human-imitation model, which most do not. A running clock overrides all of
this: the engine will not spend more time on a move than it can afford.

The ruleset and the strength setting are remembered as next time's defaults. Preferences uses
the same mode names: **Visits**, **Time per move**, **Human-like**.

### While the game runs

Click an empty point to move. The board is read-only while the engine thinks (`Thinking… 3.4k
visits` in the status line) and after the game ends. During a game the editor is forced
closed, the editing toggle is disabled, ordinary right-click is ignored, redo is disabled,
and <kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back the whole exchange rather than a document edit.
*Both (no engine)* is still a game: you play both colours, there is no engine move and no
**Resign**, and editing stays closed until the session ends.

**Clocks** appear in the play bar under the board navigation, not in the header. Black is
`●`, White is `○`, and the side to move is shown in the accent colour.

| Type | Behaviour |
|---|---|
| Absolute | main time counts down; zero loses on time |
| Fischer | the increment is added each time you complete a move |
| Byo-yomi | main time first; when it runs out you enter your first period and the clock resets to the period length. The bracketed number is how many periods remain, counting the one you are in. Completing a move inside a period resets it in full. Running a period out with none left loses on time |

Starting a byo-yomi game with zero main time drops you straight into the first period.

| | |
|---|---|
| **Pass** | <kbd>p</kbd>, or the play-bar button. Two passes in a row end the game and open scoring |
| **Undo** | <kbd>Ctrl</kbd>+<kbd>Z</kbd>, or the play-bar button, takes back the whole exchange — the engine's move and yours — cancels any search in progress, and restores both clocks exactly |
| **Resign** | the play-bar button, only when you are playing the engine. No shortcut, deliberately. The *engine* resigns on its own when its win rate has stayed below **Resign Threshold** for **Resign Streak** consecutive moves *and* the game is past the opening — both conditions, so it never gives up on move 3 |

Live analysis works during a game and will show you the engine's own thinking. Turn it off
for a fair game.

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
the record can be edited again; **Analyse Game** stops then starts whole-game analysis.

A resignation or a lost flag settles the result on its own; mirai still counts the board and
shows what the count would have been.

---

## 7. Using a remote engine

For a desktop with a GPU and a laptop without one. `mirai-server` holds KataGo open on the
desktop and lends it to the laptop over your network. Nothing leaves your network. The
connection is QUIC, so the firewall has to allow UDP, not TCP.

### On the desktop

**1. Make a token** — a long secret the laptop presents to prove it is allowed in.

```
mirai-server --generate-token
```

**2. Copy the annotated example** rather than writing a config from memory:

```
mkdir -p ~/.config/mirai
cp crates/mirai-server/server.example.toml ~/.config/mirai/server.toml
```

That path is `$XDG_CONFIG_HOME/mirai/server.toml` when the variable is set. Edit the copy:
paste the token into the `[[token]]` value, and set the KataGo binary and model. Relative
paths resolve next to the config file, so the directory can be moved as a unit. Leave the
commented keys alone unless you mean to override them. With no `[[token]]` block at all the
server starts but rejects every client.

**3. Start it.**

```
mirai-server --config ~/.config/mirai/server.toml
```

It loads every configured KataGo before it serves anyone — a large net takes seconds, and a
lazy first query would look like a hang — then prints its **certificate fingerprint**. Leave
that visible. `mirai-server --print-fingerprint` prints it again at any time. If one engine
fails to start it is logged and skipped; the server exits only if none came up.

**4. Open UDP port 9678** through the desktop's firewall. The example listens on
`0.0.0.0:9678`; `127.0.0.1` would be this machine only.

One server serves several clients from the one KataGo; it does not start a copy per person.

### On the laptop

**Preferences → Engines → Add Remote Engine**, then:

| Field | Value |
|---|---|
| **Name** | anything, e.g. `desktop` |
| **Server URL** | `mirai://192.168.1.10:9678` — must start with `mirai://` |
| **Token** | the 64 characters, masked as you type |
| **Engine name (optional)** | blank unless the desktop hosts several and you want a particular one. Blank means the server's first engine |

Press **Test Connection**. On the first successful connect the dialog is **Trust This
Server?** The body names the URL and says mirai will refuse to connect if the certificate
ever changes. The SHA-256 itself is a separate selectable monospace line, wrapped so it is
not cut off. **Cancel** is the default: Enter and Escape dismiss the dialog and do not pin
anything. **Trust** is explicit.

**Compare that line with what the server printed.** Matching → **Trust**. Not matching →
Cancel; something between the two machines is answering in the desktop's place.

Trusting *pins* that fingerprint. **Save Profile**, then select the profile from the engine
button. Everything behaves as it does locally. Changing the Server URL later discards the
pin, because a pin belongs to the address it came from, and you are asked to confirm the new
one.

### If mirai later refuses to connect

A certificate that does not match the pinned one is a hard refusal, not a warning you can
click through.

| Cause | What to do |
|---|---|
| The server's `cert.pem`/`key.pem` were deleted or regenerated, or the server was reinstalled | expected. Get the new value with `mirai-server --print-fingerprint`, then on the laptop edit the remote profile → **Test Connection** → check the selectable fingerprint → **Trust** → **Save Profile** |
| Nothing changed on the server | do not trust it. Something is intercepting the connection; check the network and the address |

---

## 8. Settings reference

Four pages. Every change is written to disk immediately; there is no Apply button. This is
the reference for what the client settings mean and for a hand-edited config. The
[README](../../README.md#requirements) does not repeat it.

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
| *remote* Server URL / Token / Engine name (optional) | — / — / blank | blank engine name means the server's first engine |
| *remote* Pinned fingerprint | not pinned | read-only; set by **Test Connection** and **Trust** |

With **Managed by mirai**, `0` in any of those four rows means mirai's own default; with
*Custom file* it means *keep what the file says*, and the row subtitles change to say so. The
generated config sets those four values and nothing else — everything else is KataGo's own
default. Three of them are there because KataGo refuses to start without
`numAnalysisThreads`, `numSearchThreadsPerAnalysisThread` and `nnMaxBatchSize`, which is why
there is a file at all rather than a handful of command-line overrides. The cache is the
exception: KataGo's own analysis-engine default is 2^23, meant for a server analysing games
in bulk, and would settle around 24 GiB on a desktop.

Automatic tuning is deliberately manual and per profile: changing a path or opening
Preferences never starts a benchmark. Wait for engine startup to finish, and finish or cancel
whole-game analysis first. Close other mirai windows; opening one while tuning stops the run.
The tuner temporarily stops this window's normal engine so another search or a second copy of
the model cannot skew the result or exhaust GPU memory. It starts KataGo once per candidate,
so first-run OpenCL kernel tuning can make the run take longer than the usual one or two
minutes. **Stop Tuning** cancels the current query and leaves the saved profile unchanged. On
success, review the measured positions, threads and batch values, then press **Save Profile**
to persist and activate them. The neural-net cache is not changed.

### Analysis

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Maximum Visits** | 1 000 000 | 1 000 – 10 000 000 | you want a live search to settle on an answer and stop using the GPU |
| **Report Interval** | 100 ms | 20 – 1 000 | the display feels busy, or the link to a remote engine is slow |
| **Suggestions Shown** | 10 | All / 1 – 50 | you want every searched move, or a cleaner board — this caps blobs and list rows together |
| **Visits per Move** | 100 | 100 – 100 000 | reviewing: 100 is quick, 5 000 is thorough |
| **Analyse on Open** | off | on / off | turn on to start a whole-game sweep whenever a record is opened, pasted or downloaded |

Changing **Maximum Visits** or **Report Interval** restarts a search that is already running,
after a brief pause so dragging or key-repeating the row does not restart on every step.
**Suggestions Shown** restarts only when the cap grows. The number is written to disk
immediately either way. The numeric rows accept typing, scrolling and the keyboard's arrow
keys. Each page ends with **Restore Defaults**. Its toast offers **Undo**. Restoring a page
does not delete engine profiles.

### Play

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Mode** | Visits | Visits / Time per move / Human-like | the same three names as New Game |
| **Visits per Move** | 800 | 1 – 1 000 000 | you want a weaker or stronger opponent |
| **Seconds per Move** | 5.0 | 0.1 – 300 | using Time per move. New Game allows up to 600 |
| **Human Model Profile** | `rank_5k` | free text | the network supports human imitation and you want another rank |
| **Temperature** | 0.00 | 0 – 2 | 0 always plays the best move; 0.2–0.4 varies the opening |
| **Resign Threshold** | 0.05 | 0 – 0.5 | 0 makes the engine play every game out |
| **Resign Streak** | 3 | 1 – 10 | it gives up too readily |
| **Default Ruleset** | Chinese | nine rulesets | used for new games |

### Appearance

| Setting | Default | Effect |
|---|---|---|
| **Coordinates** | on | letters and numbers around the board |
| **Move Numbers** | off | number every stone; the last move's number is red, so the dot is not needed |
| **Overlay** | None | one selector: **None**, **Ownership** or **Policy**. Not two switches. <kbd>o</kbd> and <kbd>y</kbd> select the same choice and turn each other off |
| **Save Analysis in SGF** | off | writes stored win rates and candidates into the SGF, so the curves survive a save and reload. Larger files; other programs ignore the extra data |

### The settings file

`~/.config/mirai/config.toml` (strictly `$XDG_CONFIG_HOME`). Plain text, editable by hand
while mirai is closed. A missing file is fine; a malformed one makes mirai fall back to a
seeded first-run configuration rather than start with half of one. Saving does not repair
that file: the error is reported and the text is left as it is. Preferences is the normal
editor. A hand-written file only needs the profile you are adding; every omitted analysis,
play and display key uses the default in the tables above.

A local profile:

```toml
active_engine = "local-default"

[[engine_profile]]
name = "local-default"
kind = "local"
katago = "/path/to/katago"
model = "/path/to/model.bin.gz"
```

A remote profile, in the same file or instead of the local one. `cert_sha256` is written
after you press **Trust**; do not invent it.

```toml
active_engine = "desktop"

[[engine_profile]]
name = "desktop"
kind = "remote"
url = "mirai://192.168.1.10:9678"
token = "…"
```

Leave `engine` out unless the server has several and you want one by name. Autosave, KataGo's
logs and the generated analysis config live under `$XDG_DATA_HOME/mirai/`.

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
| | | <kbd>g</kbd> | show/hide win-rate graph | | | | |

<kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back both players' last moves during a game; in review it
undoes the last edit — a placed stone, a mark, a comment commit — not each keystroke.
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> redoes it (disabled during a game). While a
comment or label field has focus, those keys undo typing in that field instead. The same
table is in the application under Main Menu → *Keyboard Shortcuts*.

No dedicated accelerator: switching engine profile, *Preferences*, *Keyboard Shortcuts*,
*About mirai*, *Editing Tools* and **Board Menu**. These remain reachable with standard
keyboard focus.

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
| Is the live-analysis toggle pressed in? | <kbd>Space</kbd> toggles it |
| Does the engine button show a name and version? | otherwise there is no engine. With no profile and no stored analysis, the page is **No Engine Configured** and its button opens Preferences |
| Does this move already have stored analysis? | the panel shows those numbers without an engine, and without a visits-per-second figure |
| Is **Suggestions Shown** turned down? | Preferences → Analysis |
| Is **Maximum Visits** low? | the search finishes at once and then sits still. Correct, not a hang |
| Overlays blank? | they draw nothing until the first report arrives |

When the panel is showing but this move has no report, it says which case you are in:
*"No engine"*, *"Starting …"*, the engine's failure text, or *"Analysing…"* while live
analysis waits for its first report. With an engine and nothing running it offers **Analyse
Position** (live analysis, <kbd>Space</kbd>) and **Analyse Game** (<kbd>Ctrl</kbd>+<kbd>A</kbd>).

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
| Curves are empty | no analysis is stored in the file. Run a whole-game analysis, and turn on *Save Analysis in SGF* to keep it |
| Some annotations are not drawn | mirai draws triangles, squares, circles, crosses and text labels. Others are kept in the file untouched and reappear when you save |
| Result or komi looks odd | the file's own values are used as written |
| Will not open at all | not SGF, or damaged; the toast names the problem |

Saving a file mirai opened never strips anything it did not understand.

---

Contributors and agents: start at [AGENTS.md](../../AGENTS.md). That is the entry for
invariants, the source map, the protocol, and GUI debugging. mirai is free software under the
GNU General Public License, version 3 or later.
