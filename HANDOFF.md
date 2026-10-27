# pi-gui — HANDOFF

State as of 2026-10-26 (evening). Two rounds committed: the layout/retry fixes and
the feedback batch (spinners, gear, toast pruning, multi-window, edit, loading
states). Read this before touching the code.

## What this is

Native GTK4/libadwaita desktop GUI for the [pi coding agent](https://github.com/earendil-works/pi/),
speaking pi's `--mode rpc` JSONL protocol. Goals: GNOME HIG quality, Claude/ChatGPT-desktop
layout (session sidebar, right-aligned user bubbles, plain assistant text, collapsible
reasoning, tool cards), no web stack.

- Language: Rust (gtk4-rs 0.11, libadwaita 0.9 v1_4, edition 2024)
- Layout: `src/main.rs` (app entry + CSS load), `src/app.rs` (PiWindow/State, event
  handling), `src/rpc.rs` (child process + JSONL protocol), `src/chat.rs` (transcript
  widgets), `src/session_list.rs` (sidebar), `src/style.rs` (all CSS)

## Feature batch #2 (2026-10-26 evening) — all implemented

1. **Working indicator** — spinner in the header title (visible while the agent
   runs) AND the running session's sidebar row swaps its file icon for a spinner.
   Wired in AgentStart / AgentSettled / Disconnected.
2. **Settings gear** — `emblem-system-symbolic` (the gear); the previous
   `preferences-system-symbolic` renders as a star-ish icon in current GNOME.
3. **Toast pruning** — removed per-retry toasts (persistent status line covers it),
   provider-error toast (error row covers it), "Stopped", "New chat started",
   "Model: …". Remaining toasts are failure paths only (RPC died, switch cancelled,
   no models, extension errors) plus extension-initiated notifies.
4. **Multi-session at once** — "New window" header button (window-new-symbolic) +
   every `pi-gui` launch opens an additional window. Each window owns its pi RPC
   child, sidebar and state (GtkApplication single-instance: new launches hand off
   via activate → new window in the primary process). Verified: two windows, two
   fake-pi children, independent sessions.
   NOTE: env hooks (PI_GUI_OPEN_SESSION) of the SECOND process do not reach the
   primary's new window (env is read by the primary) — per-window options need a
   different IPC if ever needed.
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

All 3 `cargo test --release` tests green (wire-format round trips + the
write-starvation regression test).

## Testing recipes

- fake-pi.py (no network) is at `/tmp/ai-agent/fake-pi.py` — recreate from the spec
  below if /tmp got wiped; `chmod +x` it or the GUI panics with PermissionDenied at
  `app.rs` spawn. It has two scripted scenarios selected by the switched session path:
  `fake-ui-test.jsonl` (thinking + text + tool + answer) and `fake-ui-retry.jsonl`
  (two retries then final failure). Sensitive to timing: the GUI auto-switches ~1s
  after start, scenarios start 2s later; when verifying transient states make the
  sleeps LONG (60s) — browser tab connect alone can take 30s+.
- Fake session files: `~/.pi/agent/sessions/--tmp-ai-agent--/fake-ui-{test,retry}.jsonl`,
  each containing one line `{"type":"session_info","name":"<display name>"}` so the
  sidebar picks a label. gio trash them when done (they pollute the real sidebar).
- Run: `PI_GUI_PI_BIN=/tmp/ai-agent/fake-pi.py PI_GUI_DIRECT=1 GDK_BACKEND=broadway
  BROADWAY_DISPLAY=:2 PI_GUI_OPEN_SESSION=<session path> target/release/pi-gui`
- Verify: browser on http://127.0.0.1:8091 — **screenshots only, never click**.

### fake-pi.py spec (recreate if lost)

Python3 script, accepts `--mode rpc`, reads JSONL stdin, writes JSONL stdout. Answers
(with the request's `id`): `get_state` (return a sessionFile path that matches NO real
sidebar row, e.g. `.../current.jsonl` — otherwise startup renames + pre-activates that
row and clicking it short-circuits), `get_available_models`,
`get_available_thinking_levels`, `get_messages`, `new_session`, `switch_session`,
`set_session_name`, `set_model`, `set_thinking_level`, `abort`, `steer` — all
`{"id","type":"response","command","success":true,...}`. On `switch_session`: store
`sessionPath`, reply success, then run the scenario for that path in a thread. On
`prompt`: reply, then run the success scenario. Event wire names: flat
`{"type": "agent_start" | "agent_settled" | "message_update" | "tool_execution_*" |
"auto_retry_start" | "auto_retry_end", ...}` with camelCase fields
(`assistantMessageEvent`, `toolCallId`, `partialResult`, `errorMessage`, `finalError`).

## Testing pitfalls (each one burned real time)

- **GtkApplication is single-instance**: launching a second `pi-gui` does NOT start a
  new process — it activates the running one (new window + extra fake-pi child in the
  SAME process). Always kill the old instance by explicit PID before relaunching.
- **killing the app crashes broadwayd** (GTK bug: `Gdk:ERROR broadway-output.c:458
  assertion failed: (!reused_node->consumed)`), and a dying daemon kills the app.
  Always restart daemon + app together; the browser tab then needs close + reopen.
- **clicks wedge or crash broadway** non-deterministically (hover-only ghosts, input
  eaten by zombie websockets from reloaded tabs, frame delivery stalls). Auto-open +
  screenshot-only verification sidesteps all of it. If interaction is unavoidable:
  exactly one tab, never reload it, move-then-click, and accept retries.
- `pkill -f <pattern>` self-matches the sandbox wrapper's own argv (its cmdline
  contains your pattern) — kill by explicit PID from `ps -eo pid,cmd` instead, and
  filter self-matches with `rg -v "chrome|AppImage"`.
- Stale checks: `kill -0 $(cat pidfile)` hits sandbox-mangled pids; verify by ps.
- `magick` geometry args containing `+` (e.g. `-crop WxH+X+Y`) get mangled by the
  sandbox — use python3/PIL for image crops.
- broadway: single browser client; display `:N` → port 8080+(N−1) unless `-p`;
  daemon arg `:N` creates socket `broadway{N+1}.socket`; client needs
  `BROADWAY_DISPLAY=:N` matching the daemon arg.
- GTK4: Button with both `label` and `icon_name` shows only the icon — compose a child
  Box (`icon_label_box()` in app.rs). AdwToast titles parse Pango markup — escape with
  `glib::markup_escape_text` (`show_toast` already does).
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
3. Possible next UI polish: markdown rendering in assistant text (currently plain
   labels — `**2 + 2 = 4**` shows literal asterisks), streamdown-style code blocks.
