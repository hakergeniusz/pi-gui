# pi-gui — HANDOFF

State as of 2026-10-26. The "still broken" round is **fixed and visually
verified**; changes are committed. Read this before touching the code.

## What this is

Native GTK4/libadwaita desktop GUI for the [pi coding agent](https://github.com/earendil-works/pi/),
speaking pi's `--mode rpc` JSONL protocol. Goals: GNOME HIG quality, Claude/ChatGPT-desktop
layout (session sidebar, right-aligned user bubbles, plain assistant text, collapsible
reasoning, tool cards), no web stack.

- Language: Rust (gtk4-rs 0.11, libadwaita 0.9 v1_4, edition 2024)
- Layout: `src/main.rs` (app entry + CSS load), `src/app.rs` (PiWindow/State, event
  handling), `src/rpc.rs` (child process + JSONL protocol), `src/chat.rs` (transcript
  widgets), `src/session_list.rs` (sidebar), `src/style.rs` (all CSS)

## Commit state

```
<see git log>  fix(ui): chat layout alignment and provider-retry visibility  <- this round
676dab8 fix(rpc): register pending response sender before writing the request
dba4ab9 feat(ui): redesign transcript, sidebar, header, and input to HIG quality
2ada722 fix(rpc): deliver commands to pi without waiting for output
3be2048 feat: working GTK4/libadwaita GUI for the pi coding agent
```

## What this round fixed (all visually verified on broadway)

1. **User bubble left-aligned** — `halign(End)` without `hexpand` is a no-op inside a
   horizontal GTK Box. Fix: `spacer()` (an `hexpand(true)` Box) before the bubble in
   `chat.rs::user_message`. ✅ Verified.
2. **Assistant header floated mid-row** — ROOT CAUSE FOUND: the π label inside the
   avatar circle had `hexpand(true)`. GTK's `compute_expand` leaks a child's expand
   flag up through its container, so the avatar circle counted as an expanding child
   of the header row and swallowed ~50% of the row's spare width (pushing "pi · time"
   to the center; the leaked pill background was invisible at 16% opacity).
   Fix: avatar labels use `halign(Center)` and NO hexpand (also the welcome avatar,
   which rendered as a 393px pill for the same reason + missing halign on the circle).
   ✅ Verified: "π pi · HH:MM" hugs the left edge everywhere.
   Debug technique that cracked it: temporary CSS classes with loud background colors
   on each header child — the blue spacer occupied only the last third, proving the
   avatar itself was the invisible wide widget.
3. **Eternal "Thinking…" on provider errors** — pi retries provider 5xx silently;
   only vanishing toots indicated anything. Now:
   - `StreamingBubble::show_retry(attempt, reason)` — persistent spinner + status
     "Retrying (attempt N) — <error>" in the streaming bubble. ✅ Verified.
   - `AutoRetryEnd success=true` → `retry_done()` restores "Thinking…"/hides.
   - `AutoRetryEnd success=false` → `chat::error_message()` appends a persistent red
     warning row (`.error-row` CSS) to the transcript. ✅ Verified
     ("Provider error after retries: … 502 Bad Gateway"), survives AgentSettled.
4. **`PI_GUI_OPEN_SESSION=<path>` env hook** — auto-opens a session after the first
   state load. Added for keyboard-free testing (broadway keyboard is dead), also
   useful as a deep-link feature later.

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
