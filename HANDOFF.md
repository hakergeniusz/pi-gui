# pi-gui — HANDOFF

State as of 2026-10-31. Five rounds committed: the layout/retry fixes, the feedback
batch (spinners, gear, toast pruning, edit, loading states), the markdown +
per-session tabs round, the centered-column + welcome round, and the GNOME
tab-bar + autosizing input + TTFT/throughput round. Read this before touching
the code.

## What this is

Native GTK4/libadwaita desktop GUI for the [pi coding agent](https://github.com/earendil-works/pi/),
speaking pi's `--mode rpc` JSONL protocol. Goals: GNOME HIG quality, Claude/ChatGPT-desktop
layout (session sidebar, right-aligned user bubbles, plain assistant text, collapsible
reasoning, tool cards), no web stack.

- Language: Rust (gtk4-rs 0.11, libadwaita 0.9 v1_4, edition 2024)
- Layout: `src/main.rs` (app entry + CSS load), `src/app.rs` (PiWindow/State, event
  handling), `src/rpc.rs` (child process + JSONL protocol), `src/chat.rs` (transcript
  widgets), `src/session_list.rs` (sidebar), `src/style.rs` (all CSS)

## Feature batch #5 (2026-10-31) — tab bar, input autosize, TTFT/throughput — all verified

1. **Tab bar autohide (GNOME HIG)** — REVERTS batch #4 item 3: `set_autohide(false)`
   is gone, the libadwaita default is back. One session → no tab bar; 2+ sessions →
   bar appears. Verified both states under Xvfb (shots S1–S4 single-session, T1
   two-session).
2. **Autosizing input card** — one line when empty, grows with its content up to
   `INPUT_MAX_HEIGHT = 180` (app.rs). `resize_input` (Rc closure) measures the
   TextView vertically at the current viewport width, applies `.min(cap)`, guarded
   by a `Cell<i32>` last-height check; connected to buffer `changed` AND the
   scrolled window's `hadjustment::changed` (width changes re-measure). Replaces
   `propagate_natural_height`/`max_content_height`, which only grew after the
   card's internal scrollbar appeared.
3. **TTFT + throughput** — `turn_started` stamped at send, `first_token_at` on the
   first thinking/text delta; TTFT = first token − send, rendered as "N.N s to
   first token" on the turn's FIRST message only (`has_header`). Throughput =
   `usage.output` tokens ÷ stream seconds when message_end carries usage, else a
   chars/4 fallback. Finalized messages get a dim "N tok/s" footer
   (`assistant_message(..., metrics)`); the thinking status line reads
   "Thinking… · N tok/s" live from 1s elapsed. `finalize_streaming(...,
   usage_tokens)` builds the string; the finalize swap ends with
   `scroll_to_bottom_settled()` (see recipes for why).
   Verified: live "Thinking… · 4 tok/s" (S3) and settled "20 tok/s" caption (S4)
   visually; the TTFT string was proven end-to-end via logs and shares the footer
   code path — the mid-turn Xvfb frame itself was stale (see recipes).

## Feature batch #4 (2026-10-30) — centered column + welcome — all verified

1. **Centered conversation column** — the transcript list AND the input card are
   wrapped in `AdwClamp` (`COLUMN_MAX_WIDTH = 760` in chat.rs): on windows wider
   than the cap both tighten and center symmetrically, Claude/ChatGPT-desktop
   style. The user's 1100-wide default window (content ~819) gets ~30px insets;
   wide windows get proportionally more. Do NOT replace the clamp with
   margin-setting closures — `connect_notify(Some("width"))` never fired in
   practice (verified: margins stayed 0 at width 743).
2. **Per-tab welcome (empty state)** — a transcript with no rows shows a centered
   greeting row (π avatar, "How can I help?", subtitle) IN THE LIST (not an
   overlay: broadway renders overlay children as a featureless slab). Toggled
   automatically by `ChatView::append` (hide) / `clear` + `remove_row` (show when
   the list empties again) — callers don't manage it.
3. **Tab bar always visible** — SUPERSEDED by batch #5 item 1 (user wanted GNOME HIG
   behavior): autohide is back; a single session shows no tab bar.
4. **Stuck tab spinner fixed** — `TabPage::set_loading` was only ever cleared by
   AgentSettled/prompt-failure; a chat that never ran a turn (fresh/empty session)
   spun forever. Now `load_state_done` and `load_history_done` set
   `page.set_loading(chat.busy)`; the switch flow sets it true again up front.

## Feature batch #3 (2026-10-27) — markdown, zero toasts, tabs — all verified

1. **Markdown rendering** — `chat.rs::markdown_to_pango`: ``` fences → `<tt>` blocks
   (language tag hidden), `#`..`####` headings → sized bold, `- `/`* ` bullets → `•  `,
   inline `` `code` `` / `**bold**` / `*italic*`. `inline_md` checks `**` BEFORE `*`
   (so `**Root:**` is bold and `a * b` stays literal); unmatched markers stay literal.
   Applied to history bubbles AND streamed finals. 2 unit tests cover it.
2. **Zero toasts** — `show_toast` deleted entirely; extension `notify` → stderr log;
   ExtensionError → stderr; stream error → stderr. Persistent in-transcript rows
   (retry status, error row) cover everything a toast used to say.
3. **Per-session tabs in one window** — `ChatSession` owns its pi RPC child, ChatView,
   streaming/tool state and TabPage; `State` holds the TabView + shared widgets.
   `create_chat_tab(Some(path))` opens a session in a new tab; closing a tab kills
   that chat's pi child. `PI_GUI_OPEN_SESSION` accepts a `;`-separated list → one tab
   per path (tests use this). Tabs run concurrently (verified: 2 tabs, 2 fake-pi
   children, independent sessions). Autoswitch toggle is per-chat via
   `ChatSession.suppress_autocompact` so programmatic row updates don't loop back.
4. **Pin-to-bottom scroll** — `ChatView` connects `changed` on the vadjustment: when
   the content height changes and the view was at the bottom (±40px), it re-pins.
   This catches late allocations (a freshly inserted bubble's height lands a frame or
   two after insert) without yanking a reader who scrolled up. `scroll_to_bottom`
   also does one idle re-scroll for the same-tick case. Root cause found via a
   temporary widget dump: the final bubble WAS in the list with h=0 at swap time,
   height landing ~2s later — the app was right, the verification viewport was lying
   (see pitfalls).
5. **Cards keep embedded content open** — `ToolBlock::set_finished` no longer collapses
   cards carrying embedded file contents/diffs (`embedded` flag); plain cards still
   collapse to keep the transcript tidy.

## Feature batch #2 (2026-10-26 evening)

1. **Working indicator** — spinner in the header title (visible while the agent
   runs) AND the running session's sidebar row swaps its file icon for a spinner.
   Wired in AgentStart / AgentSettled / Disconnected.
2. **Settings gear** — `emblem-system-symbolic` (the gear); the previous
   `preferences-system-symbolic` renders as a star-ish icon in current GNOME.
3. **Toast pruning** — removed per-retry toasts (persistent status line covers it),
   provider-error toast (error row covers it), "Stopped", "New chat started",
   "Model: …". SUPERSEDED by batch #3: ALL toasts are gone now.
4. **Multi-session at once** — SUPERSEDED by per-session tabs (batch #3 item 3);
   the "New window" header button still exists (GtkApplication single-instance:
   new launches hand off to the primary via activate).
5. **Edit messages** — hovering a user bubble reveals a pencil button left of it
   (EventControllerMotion); clicking prefills the input with the message text and
   focuses it. Re-sending appends a NEW message (pi's RPC has no history-truncation
   command; true edit-and-branch needs pi support).
6. **Slow session load feedback** — "Loading messages…" spinner row in the cleared
   transcript while switch_session/get_messages run; "Starting pi…" subtitle while
   the first get_state warms up; `[timing]` stderr logs measure switch → render.
   MEASURED (real pi, 340KB / 102-message session): get_state 10.2s (pi startup:
   provider/extension discovery), switch_session 6.6s (pi re-parses the session),
   get_messages 0.01s, GUI render ~0-1ms. The slowness is ALL pi-side; the GUI just
   needed to show progress instead of a blank screen.

## Previous round (2026-10-25/26) — root causes + verification

1. **User bubble left-aligned** — `halign(End)` without `hexpand` is a no-op inside a
   horizontal GTK Box. Fix: `spacer()` (an `hexpand(true)` Box) before the bubble in
   `chat.rs::user_message`. ✅ Verified.
2. **Assistant header floated mid-row** — ROOT CAUSE: the π label inside the
   avatar circle had `hexpand(true)`. GTK's `compute_expand` leaks a child's expand
   flag up through its container, so the avatar circle counted as an expanding child
   of the header row and swallowed ~50% of the row's spare width (the leaked pill
   background was invisible at 16% opacity). Fix: avatar labels use `halign(Center)`
   and NO hexpand (also the welcome avatar, which rendered as a 393px pill).
   Debug technique that cracked it: temporary CSS classes with loud background colors
   on each header child — the blue spacer occupied only the last third, proving the
   avatar itself was the invisible wide widget.
3. **Eternal "Thinking…" on provider errors** — pi retries provider 5xx silently;
   now: `StreamingBubble::show_retry(attempt, reason)` persistent status line,
   `retry_done()` on success, and `chat::error_message()` red row on final failure
   (survives AgentSettled). ✅ Both states verified on broadway.

All 8 `cargo test --release` tests green (3 rpc wire/bridge, 3 diff engine,
2 markdown).

## Testing recipes

- fake-pi.py (no network) is at `/tmp/ai-agent/fake-pi.py` — recreate from the spec
  below if /tmp got wiped (it DID get wiped once); `chmod +x` it or the GUI panics
  with PermissionDenied at `app.rs` spawn. Scenario selection by the ACTIVE session
  path suffix: `fake-ui-test.jsonl` (thinking + text + read/write/edit + final
  markdown answer), `fake-ui-retry.jsonl` (two retries then final failure),
  `fake-ui-empty.jsonl` (no events at all — empty transcript, welcome state shows).
- Fake session files: `~/.pi/agent/sessions/--tmp-ai-agent--/fake-ui-{test,retry}.jsonl`,
  each containing one line `{"type":"session_info","name":"<display name>"}` so the
  sidebar picks a label. gio trash them when done (they pollute the real sidebar).
- Run under **Xvfb** (broadway is abandoned — crashes, wedges, phantom slabs; don't
  revisit): `Xvfb :66 -screen 0 1280x1000x24 &` then
  `DISPLAY=:66 GDK_BACKEND=x11 PI_GUI_PI_BIN=/tmp/ai-agent/fake-pi.py
  PI_GUI_DIRECT=1 PI_GUI_OPEN_SESSION=<path>[;<path>…] target/debug/pi-gui`
  (a `;`-separated list opens one tab per session — unknown paths are filtered out).
- Capture: `gm import -window root shot.png`; crop/zoom evidence with
  `gm convert -crop WxH+X+Y +repage -resize 250% out.png`. Plain `import`, `magick`
  and `compare` resolve to the ZCode AppImage's ImageMagick with NO X11 support.
- **Xvfb does not repaint an idle UI** — a captured frame can be seconds stale
  unless something animates (spinner) or events keep damaging the region. A widget
  that "isn't rendered" in a shot may just live in a stale frame: re-shoot while a
  spinner ticks before debugging the app. When a screenshot contradicts the widget
  tree, dump the tree (names, classes, allocations, absolute rects) first; the
  loud-background-color trick (temporary CSS class with red/green background)
  split "widget missing" from "render missing" in one run.
- **The bottom band of the transcript viewport never paints on Xvfb** (last
  ~16–20px): a caption on the LAST row is allocated+mapped but invisible. Fixed by
  `margin_bottom(24)` on the transcript list plus `scroll_to_bottom_settled()`
  (idle re-scrolls with a `queue_resize` guard against a stale
  `vadjustment.upper`); don't remove either thinking the scroll "already happened".
- **Transient states need scripted timing**: a temp `PI_GUI_AUTO_PROMPT` hook in
  app.rs (set text at 30s → clear at 45s → send at 50s) plus a bash driver
  (`/tmp/ai-agent/xvfb-verify.sh` pattern: absolute T0 offsets via `wait_until`,
  `gm import` between them, hygiene block first). Delete the hook before commit.
  fake-pi runs its scenario ONLY on `prompt` (~2s lead-in); `touch` a fresh empty
  session file per run (e.g. `fake-ui-ttft.jsonl`) — gio trash test files when done.

### fake-pi.py spec (recreate if lost)

Python3 script, accepts `--mode rpc`, reads JSONL stdin, writes JSONL stdout. Answers
(with the request's `id`): `get_state` (return a sessionFile path that matches NO real
sidebar row, e.g. `.../current.jsonl` — otherwise startup renames + pre-activates that
row and clicking it short-circuits), `get_available_models`,
`get_available_thinking_levels`, `get_messages`, `new_session`, `switch_session`,
`set_session_name`, `set_model`, `set_thinking_level`, `abort`, `steer` — all
`{"id","type":"response","command","success":true,...}`. `switch_session` ONLY
records the path and replies — auto-running the scenario there double-fired and
poisoned evidence. On `prompt`: reply, then run the scenario for the current
path. Event wire names: flat
`{"type": "agent_start" | "agent_settled" | "message_update" | "tool_execution_*" |
"auto_retry_start" | "auto_retry_end", ...}` with camelCase fields
(`assistantMessageEvent`, `toolCallId`, `partialResult`, `errorMessage`, `finalError`).

## Testing pitfalls (each one burned real time)

- **GtkApplication is single-instance**: launching a second `pi-gui` does NOT start a
  new process — it activates the running one (new window + extra fake-pi child in the
  SAME process). Always kill the old instance by explicit PID before relaunching.
- broadway is dead (crashes, wedges, stale canvas slabs) — every broadway-specific
  workaround from earlier rounds was dropped; verify under Xvfb instead.
- `pkill -f <pattern>` self-matches the sandbox wrapper's own argv (its cmdline
  contains your pattern) — kill by explicit PID from `ps -eo pid,cmd` instead, and
  filter self-matches with `rg -v "chrome|AppImage"`.
- Stale checks: `kill -0 $(cat pidfile)` hits sandbox-mangled pids; verify by ps.
- `pkill`/`pgrep` here are ZCode AppImage shims (`pkill -9` → "invalid option");
  kill wedged instances with `kill -9 <pid>` from the pgrep list. Every test
  script starts with the hygiene block (pkill, sleep, kill -9 loop, abort if
  still alive): a stray hours-old instance once ate a launch via single-instance
  handoff and poisoned thread/pipe diagnostics.
- yama `ptrace_scope=1` blocks gdb attach to non-descendants, and gdbserver is
  unusable here (the env shim forks instead of execs so it tracks the wrong pid;
  a direct spawn stops at ld.so entry). Don't burn time on it — temporary
  `eprintln!` instrumentation + reading the app's stderr log is the effective
  diagnostic.
- GTK4: Button with both `label` and `icon_name` shows only the icon — compose a child
  Box (`icon_label_box()` in app.rs).
- **GTK heights land late**: a widget appended this tick reports h=0 until a later
  layout pass; anything that reads `upper()` right after an append sees stale values.
  That's what `ChatView`'s `changed`-signal pin + idle re-scroll handle — don't remove
  either thinking a scroll "already happened".
- Processes appear as `ZCode-3.16.3-linux-x64.AppImage` in some process listings; the
  Bash sandbox also re-executes commands on streaming-recovery retries — don't trust
  one pid printout, re-check with ps.

## Known issues / external factors

- **NVIDIA free endpoint 5xx loop**: the user's 17:13 hang was the *provider* failing
  ("Internal server error (attempt N)" in pi's stderr), not a GUI bug. The retry UI
  now makes it visible. Nothing to fix in the GUI; user may need a different model.
- Real keyboard on the user's actual display works (Enter-to-send logic is in
  EventControllerKey); only synthesized keyboard through broadway is dead.

## Roadmap (user's standing asks, unranked)

1. GitHub push is **offered but not approved** — private repo, explicit approval
   required before `gh repo create` / push. Secrets (`~/.zsh_secrets` env loading)
   must never enter the repo.
2. Env integration: GUI should pick up the user's `.zsh_secrets` environment (original
   requirement; `PI_GUI_PI_BIN` / `PI_GUI_DIRECT` / `PI_GUI_OPEN_SESSION` env hooks
   exist — verify what else is needed for secrets passthrough to the pi child).
3. Split view for several chats at once (user asked; deferred — tabs landed first).
4. Possible next UI polish: streamdown-style code blocks (syntax highlighting, copy
   button) on top of the markdown renderer.
