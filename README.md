# pi-gui

A native GNOME GUI for the [pi coding agent](https://github.com/earendil-works/pi), built in **Rust** with **GTK4 + libadwaita** following the latest GNOME Human Interface Guidelines.

Modeled on Claude Desktop / ChatGPT Desktop: a sidebar session list, header bar, chat transcript, and a rounded bottom input bar.

## How it works

`pi` ships with an **RPC mode** (`pi --mode rpc`) that speaks a JSONL protocol over stdin/stdout. This GUI spawns `pi` as a child process on a background thread and drives it from the GTK main loop (`glib::timeout_add_local` polls events every 50 ms), so the UI never blocks while the agent streams responses.

- `src/rpc.rs` — non-blocking RPC bridge (writer + reader threads, child process, JSONL command/response correlation, async event fan-out).
- `src/app.rs` — main window, sidebar session list, header bar, chat page, multiline input, preferences, extension-dialog handling (confirm/input/editor/select/notify).
- `src/chat.rs` — chat transcript widgets: right-aligned user bubbles, avatar + timestamp assistant rows, collapsible reasoning, tool-run cards, streaming row.
- `src/session_list.rs` — sidebar session list (icon + title + date, rounded selection pills).
- `src/style.rs` — CSS on libadwaita's palette variables (follows light/dark style and your accent color).
- `src/main.rs` — libadwaita application entry point.

## Environment & secrets

The GUI launches pi through `zsh -c 'source ~/.zshrc >/dev/null 2>&1; exec pi --mode rpc'`, so the pi child gets the **full interactive-shell environment**: the PATH entries `.zshrc` sets up (including `~/.pi/agent/bin`) and every API key exported from `~/.zsh_secrets` (which `.zshrc` sources). This works no matter how the GUI is started — terminal, app launcher, or desktop file. Output while sourcing is redirected so `.zshrc` banners (e.g. hyfetch) cannot corrupt the JSONL stream.

The pi binary is resolved in this order: `$PI_GUI_PI_BIN`, `~/.pi/agent/bin/pi`, then `pi` from PATH.

Escape hatches:

- `PI_GUI_DIRECT=1` — skip the zsh wrapper, spawn pi directly (inherits only the launching environment). Useful for debugging.
- `PI_GUI_PI_BIN=/path/to/pi` — use a specific pi binary.

## Features

- Native Adwaita theme & GNOME HIG styling; all colors derive from libadwaita's palette, so light/dark style and your accent color are followed automatically
- Welcome page + sidebar with the 40 most recent sessions from pi's store (`~/.pi/agent/sessions`), named like pi's own `/resume` picker, with date labels; clicking one loads its history (`switch_session` + `get_messages`)
- New chats are named automatically from your first message (`set_session_name`)
- Chat transcript in the Claude/ChatGPT-desktop style: right-aligned tinted user bubbles, assistant messages with a π avatar, name and timestamp, selectable text
- Reasoning ("thinking") shown as a collapsible **Reasoning** block — live-expanded while streaming, auto-collapsed when the answer starts; rendered for history too
- Tool runs as cards: icon + tool name + argument summary in the header, monospace result body, expanded while running and collapsed (click to reopen) when done, error state on failure
- Streaming assistant responses with a "Thinking…" indicator that disappears once text arrives
- **Model dropdown with built-in search** in the header bar (click and type): all models from `get_available_models` (853 across 9 providers here); selecting one sends `set_model`; the active model is selected on startup from `get_state`
- **Multiline input bar**: Enter sends, Shift+Enter inserts a newline; the bar grows with the text up to ~7 lines
- Sending while the agent is running queues a **steering message** (delivered after the current turn) instead of erroring
- Preferences window: thinking level (from `get_available_thinking_levels`) and auto-compact toggle — both wired to pi
- Extension UI dialogs (confirm / input / multi-line editor / select / notify toasts); fire-and-forget methods (`setStatus`, …) are ignored without responding; toast text is markup-escaped
- Stop (abort) button while the agent is running; auto-retry toasts on transient provider errors
- Clear handling when the pi child dies: input disabled and a "pi exited" toast, instead of a silently dead UI

## Build & run

```bash
cd ~/Projects/pi-gui
cargo build          # debug
cargo run            # launch the GUI
cargo build --release
cargo run --release  # launch release build
cargo test           # RPC wire-format serde tests + an end-to-end deadlock regression test against the real pi
```

Requires GTK4, libadwaita, Gdk4, and Pango development files (all present on Arch Linux). `pi` must be installed (`~/.pi/agent/bin/pi` is picked up automatically even if not on PATH).

### Troubleshooting

- **No window appears** — wait for the build to finish (release LTO takes a minute); try `GDK_BACKEND=x11 cargo run --release` vs `GDK_BACKEND=wayland …`; check that the window didn't open on another workspace.
- **"pi exited — RPC disconnected"** — the pi child died. Run from a terminal to see the `[pi rpc]` diagnostics on stderr. `PI_GUI_DIRECT=1` bypasses the zsh wrapper if you suspect your `.zshrc`.
- **API errors / no models** — keys come from `~/.zsh_secrets` via your `.zshrc`; verify with `zsh -ic 'env | grep -c API_KEY'`.

## Project layout

```
pi-gui/
├── Cargo.toml
├── src/
│   ├── main.rs     # GTK4/libadwaita application
│   ├── app.rs      # main window + all UI logic + RPC polling
│   ├── rpc.rs      # non-blocking pi RPC bridge (zsh env spawn)
│   ├── chat.rs     # message bubbles, streaming, tool blocks
│   ├── session_list.rs  # sidebar session list
│   └── style.rs    # Adwaita CSS
└── README.md
```

## Notes

- Built under `~/Projects/` per the repo conventions (capital `P`).
- Rust preferred per project settings; GTK4 + libadwaita chosen for latest GNOME looks.
- No secrets are read or stored by the GUI itself — the shell wrapper just sources your existing `.zshrc` so pi sees the same environment as in a terminal.
