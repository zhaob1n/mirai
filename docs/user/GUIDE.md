# mirai — user guide

English | [简体中文](GUIDE.zh-CN.md)

A Go board for Linux that talks to KataGo: live analysis, SGF review and editing, and games
against the computer. The engine can run on the same machine or on another one on your network.

---

## 1. KataGo and prerequisites

Installing from the AUR or from source is covered in the
[README](../../README.md#-installing). `mirai` is the board. `mirai-server` is optional and only
needed to put the engine on another machine ([section 7](#7-using-a-remote-engine)).

### What you need from KataGo

Supply a recent KataGo binary (`katago`) and a network (`*.bin.gz`); mirai does not download
them. It generates KataGo's analysis config from your Preferences settings. To use your
own, select **Preferences → Engines → Analysis Config → Custom file**; KataGo ships an
`analysis.cfg` in its `configs` directory.

> [!IMPORTANT]
> mirai drives KataGo's JSON **analysis** engine, never GTP — a GTP-only bot or wrapper
> script will not work.

If you already run KataGo under Lizzie, KaTrain or Sabaki, point mirai at the same binary and
network; nothing of yours is copied or modified. Boards from 2×2 to 19×19, matching a stock
KataGo build.

For stone placement and capture sounds, GTK requires GStreamer's good plugins (`gst-plugins-good`
on Arch, `gstreamer1.0-plugins-good` on Debian/Ubuntu, `gstreamer1-plugins-good` on Fedora);
without them mirai runs silently.

## 2. First run

If `katago` is on `PATH` and a network is found, mirai creates and starts a `local-default`
profile.

> [!NOTE]
> The first OpenCL start can take minutes; see
> [Troubleshooting](#the-very-first-start-takes-minutes).

Networks are looked for in this order, newest `*.bin.gz` first within each directory:

1. `$XDG_DATA_HOME/{katago,mirai}/models`
2. `~/.katago/models`
3. `{katago,mirai}/models` under each `$XDG_DATA_DIRS` entry

The first one found seeds `local-default`; the others are offered by the model chooser in
Preferences.

If no KataGo or network is found:

- The Analysis page shows **No Engine Configured** with a **Preferences** button;
  Preferences does not open by itself.
- The board and navigation still work, and stored analysis remains visible.
- Live analysis, whole-game analysis and score estimation stay greyed out until an engine is
  starting or running. Live analysis may be switched on while one starts; the others wait
  until it is ready.

### Adding a local engine

1. In **Preferences → Engines → Add Local Engine**, name the profile and choose the
   **KataGo Binary** and **Neural Network Model**. The model chooser lists discovered
   networks; the folder button accepts any other file. Both paths must exist.
2. Leave **Analysis Config** at **Managed by mirai**, or choose *Custom file* to supply your
   own. Its chooser lists `*.cfg` files in `{katago,mirai}/cfg/analysis` under
   `$XDG_CONFIG_HOME` and `$XDG_CONFIG_DIRS`.
3. Leave **Search** and **Batching and Memory** at **0** for mirai's defaults
   ([settings reference](#engines)), then **Save Profile**.

The first profile added starts immediately; later profiles do not change the active
selection. Switch with the engine button. Deleting a profile removes only its settings,
not its binary, network or config files.

## 3. The interface

| Part | What it does |
|---|---|
| **Header** | opens records from Fox, eWeiqi or Yike, files or the clipboard; starts games; switches engine profiles. The title is the file name, else `Black vs White`, else the event, else `Untitled`, with `•` for an unsaved record. The status subtitle appears only when there is something to report |
| **Board navigation** | under the board; holds the **Editing Tools** toggle |
| **Sidebar** | **Analysis**, **Moves** and **Comment** pages. <kbd>F9</kbd> toggles it. When the window is too narrow for it to sit beside the board without shrinking the board, it opens as an overlay instead |
| **Win-rate graph** | always from Black's view, while the Analysis sidebar reads the side to move. The solid curve is Black's win rate, the dashed curve Black's score lead, and the bottom bars mark blunders. Click or drag it to navigate the main line; drag the divider to resize it, and mirai remembers the height. <kbd>g</kbd> toggles it |

A game hides review navigation, graph and sidebar until it ends; the sidebar returns as you
left it.

**Editing Tools**, on the navigation bar or Main Menu → *View*, reveals the editing toolbar;
closing it returns to Play. The last board move has a red dot (or a red number if move
numbers are on). SGF triangle, square, circle, cross and text labels are drawn. See
[reviewing](#5-reviewing-a-game) for editing and [keys](#9-keyboard-reference) for shortcuts.

### Mouse on the board

| Action | Effect |
|---|---|
| Left-click (Play tool) | play a real move for the side to play — captures, legality, move number |
| Right-click (Play tool) | take back the current node: delete it and its continuation, then return to its parent. Undo restores the deleted branch; the root cannot be deleted. This is not a menu |
| Left-click / right-click (black or white setup tool) | use the selected colour / the opposite colour: place on an empty point, replace an opposite stone, or remove a matching stone. No captures or move numbers; marks are preserved |
| Left-click with a mark tool | apply that tool; right-click does nothing |
| Left-click during scoring | toggle that group alive/dead |
| Scroll wheel | browse back / forward one move without deleting anything |
| Hover a candidate blob | preview its variation. Non-Play tools clear this preview |
| Hover an intersection (Play or setup tool) | show a translucent stone where a left-click would place one: the side to play on a legal point, or the setup colour on an empty point. During a game it appears only on your turn |

## 4. Analysing a position

<kbd>Space</kbd> starts or stops live analysis at the current position; navigating starts
a new search. The live visit cap, report interval and candidate limit are in
[settings](#analysis). Stored analysis is visible without an engine, but visits per second
appear only during a live search.

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
reading.**

🟦 cyan → 🟩 mint and green → 🟨 yellow → 🟧 orange → 🟥 red

The engine's pick is cyan; a worse move walks towards red, through the same warm colours the
blunder strip uses. Loss is the engine's own combined reading of win rate and score, not the
visit count and not the rank. Fewer than ten visits is grey and faint: unknown, not good or
bad. The pick is never grey. Why the list order can disagree with Win, Score and Visits is
measured in [Candidate order versus candidate colour](../dev/CANDIDATE_COLOUR.md).

**The white outline marks the move the record plays next.** Standing on move 57, the outlined
blob is move 58. If that move is not among the candidates — the search never went there, or
it falls past **Suggestions Shown** — the outline sits on a dim empty disc. At the end of the
record there is no outline.

**Suggestions Shown** limits both blobs and list rows; increasing it restarts a live
search, while decreasing it only redraws. See the [range and defaults](#analysis).

### The candidate list

| Column | Meaning |
|---|---|
| **#** | the engine's rank, not its visit count; coloured like the blob's loss grade |
| **Move** | the point |
| **Win**, **Score** | from the side to move's perspective |
| **Visits** | how much of the search went there |
| **Loss** *(off by default)* | what the move gives away against the pick; `—` if a saved record lacks it |
| **Prior** *(off by default)* | the raw network's pre-search preference |

Enable **Loss** and **Prior** with Main Menu → *View* → *Loss and Prior Columns*, or by
right-clicking a column heading. High prior with few visits means the network liked a move
that search did not; low prior with many visits means search found something the network
nearly missed.

- The list starts in engine order. Sorting changes only list order, never blobs, rank
  numbers or the move the engine would play; sort by **#** to restore engine order.
- Selecting a row pins its variation through later reports until you navigate away.
  Double-clicking or pressing <kbd>Enter</kbd> plays the move.
- Hover the visits line above the list for KataGo's final-score spread; a cached reading has
  no search speed.

### Previewing a variation

Hover any candidate blob: the board redraws with that variation played out and numbered from
the current position. Move away and the real position returns. Nothing is written to the
record.

### Overlays

Both read from the same analysis, so they need a report (live, or one cached on this move).
Preferences → General → Board → **Overlay** is one choice: **None**, **Ownership** or
**Policy**. <kbd>o</kbd>, <kbd>y</kbd> and the View menu select that same choice; turning one
on turns the other off.

| Overlay | Key | Reading it |
|---|---|---|
| **Ownership** | <kbd>o</kbd> | shades each point by who is expected to own it at the end — dark for Black, light for White, stronger shade = more certain, unshaded = genuinely unsettled. Groups the engine has written off show as enemy territory while their stones are still on the board |
| **Policy** | <kbd>y</kbd> | shades each point by what the raw network wants to play there, before any search. Comparing it with the searched candidates shows where search disagrees with instinct |

### One-off score estimate

<kbd>Ctrl</kbd>+<kbd>E</kbd> runs a short 400-visit search, derives the dead stones from the
ownership map, counts the board under the game's own rules and komi, and shows the count
next to KataGo's Black-positive score lead. Works whether or not live analysis is on.

## 5. Reviewing a game

### Opening records

<kbd>Ctrl</kbd>+<kbd>O</kbd>, *Open File…* in the download button's arrow menu, or a file on
the command line. If the file holds several games a dialog lists them — players, size,
moves, result, date — and you choose. <kbd>Ctrl</kbd>+<kbd>V</kbd> pastes a record from the
clipboard, <kbd>Ctrl</kbd>+<kbd>C</kbd> copies the current one out. Anything mirai does not
understand in an SGF file is kept verbatim and written back, so files from other programs
survive a round trip.

Opening a file while mirai is running — from your file manager, or another
`mirai game.sgf` on the command line — gives that record its own window rather than
replacing what you are looking at; passing several files at once opens one window each. The
windows are independent, but they share one KataGo: a second window costs no extra GPU
memory and no second startup wait, and the engine shuts down when the last window using it
closes.

### Downloading a game record

Click the main **Download Game Record** half of the split button, or press
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd>. Choose the server at the top of the dialog,
then search:

| Server | Search by | Lists |
|---|---|---|
| **Fox** | an exact Fox nickname or numeric UID | at most the latest 200 public records, the service's fixed history window. Players who hide their records are not bypassed |
| **eWeiqi** | a player's name or nickname, matched in part and as eWeiqi spells it (a Korean professional's name is often in traditional characters) | the latest 200 matches. eWeiqi shows strangers only its catalogue of tournament records, not an account's own games, so this finds the professional and tournament games it publishes |
| **Yike** | a nickname, a Yike number such as `CGF00001`, a numeric account id, or a professional's name | the latest 100 games. A professional's name opens Yike's game library; a nickname opens that member's online games. Nicknames are not unique: when several players match, the dialog lists them, ten to a page, and you pick one. The pick is kept with the search; to choose again, remove the search from the recent list |

- Games show ten to a page; the arrows under the list turn to newer and older games.
- Every search is kept with its games. Searching for the same name on the same server again
  answers at once with what the server sent then; the refresh button beside the list asks
  again. A saved search still shows its games when the server cannot be reached.
- With the search box empty, or while you type, the dialog lists your recent searches on the
  chosen server, ten to a page; pick one to see its games, or remove it with its trash
  button. Opening the dialog again restores the last search and page.
- Click a game, or press <kbd>Enter</kbd> on it, to download and open it; <kbd>Enter</kbd> in
  the search box searches.

Each server's dialect is normalised on import: Fox's quarter-point Chinese komi and handicap
stones written as a run of opening nodes, eWeiqi's own record format and its commentary,
Yike's results written in Chinese. The result has no local backing file: it is named after
its players, `柯洁 vs 申真谞 •`, and **Save** therefore asks where to store it.

### Navigating

Use the [keys](#9-keyboard-reference), board scroll wheel or slider, graph, or Moves page.
Clicking the graph scrubs the main line.

### Curves and the blunder strip

The curves have gaps where moves have no stored analysis; a whole-game analysis fills them
in.

The blunder strip has one bar per move, from the point of view of whoever played it. A bar is
drawn only when both that move and the position before it have been analysed — an
unanalysed stretch is a gap, not a mistake.

| Win rate lost | Bar |
|---|---|
| up to 2 % | nothing |
| 2 – 5 % | 🟨 yellow |
| 5 – 10 % | 🟧 orange |
| over 10 % | 🟥 red |

### Whole-game analysis

<kbd>Ctrl</kbd>+<kbd>A</kbd> sweeps the main line at **Visits per Move** (100 by default).
**Cancel** keeps completed analyses; the curves fill in as results arrive.

A **Blunders** list uses stored analyses and updates with the record. Each row shows the
mover, move number, win rate lost, played move and engine choice if stored. Click a row to
jump there; collapse the list without losing its contents. An analysed record shows its
blunders even without a running engine, though starting a new sweep requires one.

### Editing the record

**Editing tools.** The toolbar above the board starts open; hide or show it with **Editing
Tools** on the navigation bar or the View menu. It contains undo/redo, **Switch Side to
Play**, Play, setup and marks. Closing it returns to Play without adding an undo step. During
a game it stays closed and disabled.

**Variations and the move tree.** Playing before the end creates a variation, leaving the
original continuation intact. In Moves, the main line runs downwards with variations to
the right; a hollow node is a start or setup position, a bar through one is a pass and the
current node has a ring. Click a node to navigate; right-click one for *Set as Main Line*
(offered only off the main line) and *Delete Branch* (not offered on the start).

| Operation | How |
|---|---|
| Promote a variation to the main line | Right-click its node in Moves → *Set as Main Line*, or <kbd>Ctrl</kbd>+<kbd>↑</kbd> for the current node |
| Delete this move and everything after it | Right-click in Play, <kbd>Delete</kbd>, or right-click its node in Moves → *Delete Branch* |
| Undo / redo the last edit | <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> |

The start of the game cannot be deleted. Setup on a node that already has a move or a
continuation adds a new variation; further setup clicks stay on that new leaf.

**Stone tools.** The Play tool's icon shows the side to move. **Switch Side to Play** (the
⇄ button beside it, or <kbd>t</kbd>) hands the move to the other side; the record stores
that as `PL`. Choose black or white setup to place, remove or replace stones without
changing the turn. Right-click uses the opposite colour.

**Comments and marks.** Edit a move's comment in **Comment**. It saves when you navigate
away, leave the field or save the record; the typing session is one undo. A second click of
the same mark removes it; a different mark replaces it. Empty label text deletes a label.
The eraser removes marks and labels, not stones.

## 6. Playing

<kbd>Ctrl</kbd>+<kbd>N</kbd> or the **New Game** button. To wipe the current record back to
an empty board without starting a game against the engine, use Main Menu → *Clear Board* or
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd>. Size, rules and komi stay; stones, comments
and the file path do not.

### The New Game dialog

Choose a board, ruleset, colour and optional clock, then **Start Game**. *Both (no engine)*
lets you play both sides and hides engine strength. A running clock limits how long the engine
can think regardless of strength. The ruleset and strength choice become the next game's
defaults. If the current network lacks a saved Human-like profile, this game uses Visits
without changing the saved setting.

See the [settings reference](#play) for board, clock, ruleset and strength choices and defaults.

### While the game runs

- Click an empty point to move.
- The board is read-only while the engine thinks (`Thinking… 3.4k visits` in the status
  line), while its turn is stalled, and after the game ends.
- If the engine cannot move — none is running, or the search fails — the status line says
  why. **Retry** in the play bar asks again; starting an engine does that on its own.
  **Undo** and **Resign** still work, and the board stays read-only until it is your turn.
- The editor is forced closed, the editing toggle is disabled, ordinary right-click is
  ignored, redo is disabled, and <kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back the whole exchange
  rather than a document edit.
- *Both (no engine)* is still a game: you play both colours, there is no engine move and no
  **Resign**, and editing stays closed until the session ends.

You can navigate back to review earlier moves during a game. The clock and engine continue at
the latest played move, not the viewed position; when the engine replies, the board follows
the new move. Playing on an earlier position is disabled, and **Undo** retracts the latest
exchange rather than the position you were reviewing.

> [!TIP]
> Live analysis works during a game and will show you the engine's own thinking. Turn it
> off for a fair game.

### Clocks

Clocks appear in the play bar under the board navigation, not in the header. Black is `●`,
White is `○`, and the side to move is shown in the accent colour.

| Type | Behaviour |
|---|---|
| Absolute | main time counts down; zero loses on time |
| Fischer | the increment is added each time you complete a move |
| Byo-yomi | main time first; when it runs out you enter your first period and the clock resets to the period length. The bracketed number is how many periods remain, counting the one you are in. Completing a move inside a period resets it in full. Running a period out with none left loses on time |

Starting a byo-yomi game with zero main time drops you straight into the first period.
A Fischer game with zero main time starts with one increment on the clock.

### Pass, retry, undo, resign

| Action | How |
|---|---|
| **Pass** | <kbd>p</kbd>, or the play-bar button. Two passes in a row end the game and open scoring |
| **Retry** | the play-bar button, only when the engine's turn stalled. Asks for the move again |
| **Undo** | <kbd>Ctrl</kbd>+<kbd>Z</kbd>, or the play-bar button, takes back the whole exchange — the engine's move and yours — cancels any search in progress, and restores both clocks exactly |
| **Resign** | the play-bar button, only when you are playing the engine. No shortcut, deliberately. The *engine* resigns when its win rate stays below **Resign Threshold** for **Resign Streak** consecutive turns and the move number exceeds one quarter of the board's points |

### Scoring

After two passes mirai searches briefly, estimates dead stones from ownership and counts
under the game's rules and komi. The scoring view fades dead stones and marks territory
with squares in the owner's colour.

**Click any group to toggle it alive or dead** once the count is shown; while the status
line says `Counting…` the board is read-only. The count updates locally, without another
engine query; use it to correct a misjudged group or seki.

| Button | Effect |
|---|---|
| **Close** | keeps counting |
| **Review Game** | ends the session so the record can be edited |
| **Analyse Game** | ends it and starts whole-game analysis |

A resignation or a lost flag settles the result on its own; mirai still counts the board and
shows what the count would have been.

## 7. Using a remote engine

`mirai-server` runs KataGo on another machine — one with more compute, or one you need to
reach remotely — and mirai connects to it over the network. With both machines on your LAN,
analysis stays on that network.

> [!IMPORTANT]
> The connection uses QUIC, so the firewall must allow UDP, not TCP.

### On the desktop

**1. Make a token** — a long secret the laptop presents to prove it is allowed in.

```
mirai-server --generate-token
```

**2. Copy the annotated example** rather than writing a config from memory:

```
mkdir -p ~/.config/mirai
cp crates/mirai-server/server.example.toml ~/.config/mirai/server.toml
chmod 600 ~/.config/mirai/server.toml
```

That path is `$XDG_CONFIG_HOME/mirai/server.toml` when the variable is set. Paste the
generated token into `[[token]]` (the example's all-zero token is rejected) and set
the KataGo binary and model. Relative paths resolve next to the config file. A token longer
than 256 bytes stops the server starting.

> [!CAUTION]
> The token is a bearer secret: keep the file readable only by your account. The server
> warns if other users can read it.

**3. Start it.**

```
mirai-server --config ~/.config/mirai/server.toml
```

It loads every configured KataGo before it serves anyone — a large net takes seconds, and a
lazy first query would look like a hang — then prints its **certificate fingerprint**. Leave
that visible. `mirai-server --print-fingerprint` prints it again at any time. If one engine
fails to start it is logged and skipped; the server exits only if none came up.

If a running KataGo later exits, new requests fail until the server restarts that
engine in the background; in-flight requests are not replayed.

**4. Open UDP port 9678** through the desktop's firewall. The example listens on
`0.0.0.0:9678`; `127.0.0.1` would be this machine only.

One server serves several clients from the one KataGo; it does not start a copy per person.
KataGo analyses `analysis_threads` positions at once and queues the rest: an engine move or
live analysis goes ahead of a whole-game analysis, otherwise first come, first served.
`max_subs` (default 64) only caps how many requests one token may have queued or running.

### On the laptop

1. In **Preferences → Engines → Add Remote Engine**, enter a name, the server address
   (`192.168.1.10`, or `192.168.1.10:9678`; the port defaults to 9678), and the generated
   token. If the server hosts several engines, name one; otherwise leave
   **Engine Name (Optional)** blank to use the first.
2. Press **Test Connection** and compare the fingerprint in **Trust This Server?** with the
   one the server printed.
3. If they match, press **Trust**, then **Save Profile**, and select it from the engine
   button. If they differ, cancel and investigate the network and address.

The token is sent only after you trust the matching fingerprint. Selecting an unpinned
profile asks the same question; cancelling leaves it unconnected. Pointing the Server
Address at another server clears its old pin; respelling the same one (adding or
dropping `:9678`) keeps it.

### If mirai later refuses to connect

> [!WARNING]
> A certificate that does not match the pinned one is a hard refusal, not a warning you can
> click through.

| Cause | What to do |
|---|---|
| The server's `cert.pem`/`key.pem` were deleted or regenerated, or the server was reinstalled | expected. Get the new value with `mirai-server --print-fingerprint`, then on the laptop edit the remote profile → **Test Connection** → check the selectable fingerprint → **Trust** → **Save Profile** |
| Nothing changed on the server | do not trust it. Something is intercepting the connection; check the network and the address |

For other connection failures, check the server address and UDP 9678, that the
server listens beyond `127.0.0.1`, and that its `[[token]]` value matches exactly.
Without a token block the server starts but refuses every client.

## 8. Settings reference

Preferences saves changes automatically. This section is the reference for client settings
and hand-edited configs.

### Engines

| Setting | Default | Notes |
|---|---|---|
| Engine Profiles | one, if a KataGo was found | radio button = active engine; pencil edits, bin deletes |
| *local* Name / KataGo Binary / Neural Network Model | — | both paths must exist to save; the model row's list button holds every discovered network, the folder button any other file |
| *local* Analysis Config | Managed by mirai | *Custom file* reveals the config row, whose list button holds every discovered analysis config; tuning settings come from that file unless you override the two thread counts |
| *local* Positions in Parallel | 0, meaning 4 | `numAnalysisThreads`: positions searched at once. Four keeps a whole-game sweep and a cursor move from queueing behind each other |
| *local* Threads per Position | 0, meaning 16 | `numSearchThreadsPerAnalysisThread`: how hard one position is searched. Raise on a many-core CPU, but the returns fall off past 16 |
| *local* GPU Batch Size | 0, meaning 64 | `nnMaxBatchSize`. Wants to be at least positions × threads. Hidden while a custom config is selected |
| *local* Neural-Net Cache | 0, meaning 20 | `nnCacheSizePowerOfTwo`: 2^20 cached evaluations, roughly 3 GiB once warm. 0 is the default; the next step is 14. Hidden while a custom config is selected |
| *local* Automatic Tuning | off | **Tune…** measures the selected binary and model, updates the three performance rows, and waits for **Save Profile** before applying them. Managed configs only |
| *remote* Server Address / Token / Engine Name (Optional) | — / — / blank | blank engine name means the server's first engine |
| *remote* Pinned Fingerprint | not pinned | read-only; set by **Test Connection** and **Trust** |

With **Managed by mirai**, `0` in any of those four rows means mirai's own default; with
*Custom file* it means *keep what the file says*, and the row subtitles change to say so.
Managed configs set the four values above. Custom configs keep their tuning settings except
for any nonzero thread-count overrides. In both modes, mirai fixes reporting to Black's
perspective and controls logging.

> [!WARNING]
> In a custom config, set `nnCacheSizePowerOfTwo` explicitly: KataGo's analysis default is
> 2^23, roughly 24 GiB once warm.

#### Automatic tuning

Tuning is deliberately manual and per profile: changing a path or opening Preferences never
starts a benchmark.

- **Before you start**, wait for engine startup to finish, finish or cancel whole-game
  analysis, and close other mirai windows; opening one while tuning stops the run.
- **While it runs**, the tuner stops this window's normal engine so another search or a
  second copy of the model cannot skew the result or exhaust GPU memory. It starts KataGo
  once per candidate, so first-run OpenCL kernel tuning can make the run take longer than the
  usual one or two minutes. **Stop Tuning** cancels the current query and leaves the saved
  profile unchanged.
- **On success**, review the measured positions, threads and batch values, then press
  **Save Profile** to persist and activate them. The neural-net cache is not changed.

### Analysis

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Maximum Visits** | 1 000 000 | 1 000 – 10 000 000 | you want a live search to settle on an answer and stop using the GPU |
| **Report Interval** | 100 ms | 20 – 1 000 | the display feels busy, or the link to a remote engine is slow |
| **Suggestions Shown** | 10 | All / 1 – 50 | you want every searched move, or a cleaner board — this caps blobs and list rows together |
| **Visits per Move** | 100 | 100 – 100 000 | reviewing: 100 is quick, 5 000 is thorough |
| **Analyse on Open** | off | on / off | turn on to start a whole-game sweep whenever a record is opened, pasted or downloaded |
| **Save Analysis in SGF** | off | on / off | turn on to write stored win rates and candidates into saved and autosaved SGF, so the curves survive a save and reload. Larger files; other programs ignore the extra data |

Changing **Maximum Visits**, **Report Interval**, or a larger **Suggestions Shown**
restarts a search that is already running, after a brief pause so dragging or
key-repeating the row does not restart on every step. Shrinking **Suggestions Shown**
only redraws: the engine already sent every move the board now keeps. Both changes are saved
automatically. The numeric rows accept typing, scrolling and the keyboard's arrow keys.
Each page ends with **Restore Defaults**. Its toast offers **Undo**. Restoring a page
does not delete engine profiles.

### Play

| Setting | Default | Range | Change it when |
|---|---|---|---|
| **Mode** | Visits | Visits / Time per move / Human-like | Visits controls search work; Time per move fixes the time; Human-like imitates a rank and requires a compatible network — without a clock it searches 40 visits a move |
| **Visits per Move** | 800 | 1 – 1 000 000 | strength in Visits mode |
| **Seconds per Move** | 5.0 | 0.1 – 600 | fixed time per move |
| **Human Model Profile** | `rank_5k` | free text | rank to imitate with a human model |
| **Temperature** | 0.00 | 0 – 2 | 0 always plays the best move; 0.2–0.4 varies the opening |
| **Resign Threshold** | 0.05 | 0 – 0.5 | 0 makes the engine play every game out |
| **Resign Streak** | 3 | 1 – 10 | it gives up too readily |
| **Default Ruleset** | Chinese | nine rulesets | preselected in **New Game**; starting a game saves its ruleset here |

#### New Game board and clock

These fields belong to the **New Game** dialog; the strength rows above use the Play
defaults. A running clock bounds the engine's thinking time.

| Field | Choices / default | Effect |
|---|---|---|
| **Size** | 9×9, 13×13, **19×19**, Custom (2–19) | Custom reveals **Custom Size** |
| **Handicap** | **None**, 2–9 stones | standard Black placements; only on odd square boards of 7×7 and up |
| **Komi** | −150 to 150 by halves | follows the ruleset until changed; handicap sets it to 0.5 |
| **Rules** | Chinese by default | the nine rulesets and their default komis are below |
| **You Play** | **Black**, White, Both (no engine) | Both hides engine strength; switching back preserves its value |
| **Time control Type** | **None**, Absolute, Byo-yomi, Fischer increment | choose the clock format |
| **Main Time (Minutes)** | 20 | shown for any timed game |
| **Byo-yomi Periods** | 5 (1–25) | byo-yomi only |
| **Seconds per Period** | 30 (1–600) | byo-yomi only |
| **Increment (Seconds)** | 10 (0–600) | Fischer only |

| Ruleset | Default komi | Ruleset | Default komi |
|---|---|---|---|
| Tromp-Taylor | 7.5 | Stone Scoring | 7.5 |
| Chinese | 7.5 | AGA | 7.5 |
| Chinese (OGS) | 7.5 | AGA (Button) | 7.0 |
| Japanese | 6.5 | New Zealand | 7.0 |
| Korean | 6.5 | | |

### General

| Setting | Default | Effect |
|---|---|---|
| **Coordinates** | on | letters and numbers around the board |
| **Move Numbers** | off | number every stone; the last move's number is red, so the dot is not needed |
| **Overlay** | None | one selector: **None**, **Ownership** or **Policy**. Not two switches. <kbd>o</kbd> and <kbd>y</kbd> select the same choice and turn each other off |
| **Stone Sounds** | 100% | volume of the click whenever a stone is placed — by you, the AI, or stepping forward one move — and of the stones dropping into the lid when the move captures, more of them for a bigger capture. Drag to the left end, **Muted**, to turn them off. Jumps, passes and going back are silent |

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

## 9. Keyboard reference

The same tables are in the application under Main Menu → *Keyboard Shortcuts*.

### Navigation

| Key | Action |
|---|---|
| <kbd>Home</kbd> / <kbd>End</kbd> | first / last move |
| <kbd>←</kbd> <kbd>→</kbd> | one move |
| <kbd>Page Up</kbd> / <kbd>Page Down</kbd> | ten moves |
| <kbd>↑</kbd> <kbd>↓</kbd> | variations |

### Analysis and view

| Key | Action |
|---|---|
| <kbd>Space</kbd> | live analysis on/off |
| <kbd>Ctrl</kbd>+<kbd>A</kbd> | analyse whole game |
| <kbd>Ctrl</kbd>+<kbd>E</kbd> | estimate score |
| <kbd>o</kbd> / <kbd>y</kbd> | ownership / policy overlay |
| <kbd>c</kbd> / <kbd>n</kbd> | coordinates / move numbers |
| <kbd>F9</kbd> / <kbd>g</kbd> | show/hide sidebar / win-rate graph |

### Game and editing

| Key | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>N</kbd> | new game |
| <kbd>p</kbd> | pass |
| <kbd>Ctrl</kbd>+<kbd>Z</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> | undo / redo the last edit |
| <kbd>Delete</kbd> | delete branch |
| <kbd>Ctrl</kbd>+<kbd>↑</kbd> | set as main line |
| <kbd>t</kbd> | switch side to play |

<kbd>Ctrl</kbd>+<kbd>Z</kbd> takes back both players' last moves during a game; in review it
undoes the last edit — a placed stone, a mark, a comment commit — not each keystroke.
<kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Z</kbd> redoes it (disabled during a game).

### Files and records

| Key | Action |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>O</kbd> | open |
| <kbd>Ctrl</kbd>+<kbd>S</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd> | save / save as |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>V</kbd> | copy / paste record |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>O</kbd> | download a game record |
| <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>N</kbd> | clear board |

### When a text field has focus

**Typing wins.** While a comment, label or search field has focus, the keys it types and
edits with go to it: letters, Space, arrows, <kbd>Delete</kbd>, and
<kbd>Ctrl</kbd>+<kbd>A</kbd>/<kbd>C</kbd>/<kbd>V</kbd>/<kbd>Z</kbd> — so <kbd>p</kbd> in a
comment is a letter, not a pass, and <kbd>Ctrl</kbd>+<kbd>Z</kbd> undoes typing. Click the
board, the graph or the move tree to hand those keys back. <kbd>Ctrl</kbd>+<kbd>S</kbd>,
<kbd>Ctrl</kbd>+<kbd>O</kbd>, <kbd>Ctrl</kbd>+<kbd>N</kbd>, <kbd>Ctrl</kbd>+<kbd>E</kbd> and
<kbd>F9</kbd> work everywhere. With a button focused, <kbd>Space</kbd> presses that button,
as everywhere in GNOME.

No dedicated accelerator: switching engine profile, *Preferences*, *Keyboard Shortcuts*,
*About mirai* and *Editing Tools*. These remain reachable with standard keyboard focus.

## 10. Files mirai writes

| Path | What |
|---|---|
| `~/.config/mirai/config.toml` | settings and engine profiles, remote tokens included, so it is written readable by you alone (`0600`, in a `0700` directory when mirai creates it) |
| `~/.local/share/mirai/autosave-*.sgf` | the record each open window is looking at, one file per window |
| `~/.local/share/mirai/kifu-searches.json` | your last 20 searches on Fox, eWeiqi and Yike with their game lists, shown again without asking the server when you search for the same name |
| `~/.local/share/mirai/katago-logs/` | KataGo's own logs, one file per engine start, and the generated `katago-analysis-*.cfg`; also `mirai-server`'s, for an `[[engine]]` without `log_dir`. Created private; a directory for generated configs is refused if another user owns it or can write to it, or to a directory on the way to it (a sticky one such as `/tmp` excepted) |
| `~/.config/mirai/server.toml` | `mirai-server`'s settings, on the machine running it |

(`$XDG_CONFIG_HOME` and `$XDG_DATA_HOME` are honoured if set.)

- **Autosave** runs every 30 seconds, and only when there is something worth keeping — a
  move, a setup stone, a mark, an explicit side to play, or a comment; a blank board is never
  saved.
- **Closing a window deletes its autosave**, so a file still there on the next start is one a
  crash left behind, and that is what mirai offers to restore. The autosave is not your file:
  restoring it does not make it the target of a plain Save.
- **KataGo's logs** accumulate and can be deleted at any time, as can the generated analysis
  config — mirai writes it again whenever its contents would change.
- **The saved searches** can be deleted; the next lookup starts the list again.

Nothing else is written; your own KataGo installation, model and any analysis config you
supplied are never modified.

## 11. Troubleshooting

### The engine will not start

If the profile fails and the engine button reads “*name* Unavailable”, check that the
binary and model still exist in **Preferences → Engines**. It must be KataGo's JSON
analysis engine, not GTP ([setup](#1-katago-and-prerequisites)). For GPU errors or a
mismatched model, read `~/.local/share/mirai/katago-logs/`; use a binary and model that
work together.

### The very first start takes minutes

Possibly ending in `katago did not answer within 180s`.

- **Cause:** an OpenCL KataGo tunes itself to your GPU the first time it runs on a given
  board size. This genuinely takes minutes, once.
- **Fix:** wait it out, watching `~/.local/share/mirai/katago-logs/`. If it does time out,
  run `katago benchmark -model … -config …` once from a terminal so the tuning completes with
  no clock on it; afterwards mirai starts in seconds. Do the same on the server for a remote
  engine.

### Analysis does not appear

Press <kbd>Space</kbd> to start [live analysis](#4-analysing-a-position). If no profile
is configured, add one in **Preferences → Engines**. Stored results show without an
engine; an overlay needs at least one report. If search stops quickly or candidates
are missing, check **Maximum Visits** and **Suggestions Shown** in [settings](#analysis).
While waiting for the first report, the page says *Analysing…*; an engine startup
error appears there too.

### The remote connection is refused

See [remote setup and connection failures](#7-using-a-remote-engine) for URL, UDP,
token and fingerprint checks.

### "mirai did not shut down cleanly"

A window closed without cleaning its autosave. **Restore** loads it as an untitled
record (the first Save asks for a location); **Discard** deletes it. If several
windows were open, new windows offer their autosaves, most recent first.

### An SGF from another program looks wrong

| Symptom | Cause / fix |
|---|---|
| Only one game opened from a multi-game file | reopen and choose another in the [game list](#opening-records) |
| Curves are empty | run a whole-game analysis, then enable [Save Analysis in SGF](#analysis) to preserve them |
| Some annotations are not drawn | unsupported properties are preserved, even if mirai does not display them |
| Result or komi looks odd | the file's own values are used as written |
| Will not open at all | not SGF, or damaged; the toast names the problem |

### No stone sounds

GTK plays stone placement and capture audio through GStreamer; the plugins it needs are in
[section 1](#1-katago-and-prerequisites). Then check that the **Stone Sounds** slider in
Preferences → General is not muted.

---

Contributors and agents: start at [AGENTS.md](../../AGENTS.md). That is the entry for
invariants, the source map, the protocol, and GUI debugging. mirai is free software under the
GNU General Public License, version 3 or later.
