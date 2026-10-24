# pi-gui

A native GNOME GUI for the [pi coding agent](https://github.com/earendil-works/pi), built in **Rust** with **GTK4 + libadwaita** following the latest GNOME Human Interface Guidelines.

Modeled on Claude Desktop / ChatGPT Desktop: a sidebar session list, header bar, message bubbles, and a bottom input area.

## How it works

`pi` ships with an **RPC mode** (`pi --mode rpc`) that speaks a JSONL protocol over stdin/stdout. This GUI spawns `pi` as a child process on a background thread and drives it from the GTK main loop (`glib::timeout_add_local` polls events every 50 ms), so the UI never blocks while the agent streams responses.

- `src/rpc.rs` — non-blocking RPC bridge (child process + JSONL command/response correlation + async event fan-out).
- `src/app.rs` — main window, sidebar session list, header bar, chat page, input, preferences, extension-dialog handling (confirm/input/editor/select/notify).
- `src/chat.rs` — user/assistant message bubbles, streaming bubble with live thinking text, tool-call blocks, scrollable transcript.
- `src/session_list.rs` — sidebar session list with active-row highlighting.
- `src/style.rs` — Adwaita-driven CSS (accent-color user bubbles, card-surface assistant bubbles, rounded input, toasts).
- `src/main.rs` — libadwaita GTK4 application entry point.

## Environment & secrets

The GUI launches pi through `zsh -c 'source ~/.zshrc >/dev/null 2>&1; exec pi --mode rpc'`, so the pi child gets the **full interactive-shell environment**: the PATH entries `.zshrc` sets up (including `~/.pi/agent/bin`) and every API key exported from `~/.zsh_secrets` (which `.zshrc` sources). This works no matter how the GUI is started — terminal, app launcher, or desktop file. Output while sourcing is redirected so `.zshrc` banners (e.g. hyfetch) cannot corrupt the JSONL stream.

The pi binary is resolved in this order: `$PI_GUI_PI_BIN`, `~/.pi/agent/bin/pi`, then `pi` from PATH.

Escape hatches:

- `PI_GUI_DIRECT=1` — skip the zsh wrapper, spawn pi directly (inherits only the launching environment). Useful for debugging.
- `PI_GUI_PI_BIN=/path/to/pi` — use a specific pi binary.

## Features

- Native Adwaita theme & GNOME HIG styling (sidebar, header bar, suggested-action/destructive buttons, toasts)
- Welcome page + sidebar with the 40 most recent sessions from pi's store (`~/.pi/agent/sessions`), named like pi's own `/resume` picker; clicking one loads its history (`switch_session` + `get_messages`)
- New chats are named automatically from your first message (`set_session_name`)
- Streaming assistant responses with dimmed live thinking text and a typing indicator; final answer replaces the stream when the message completes
- Tool-call blocks rendered inline, with error state
- **Model dropdown + search box** in the header bar: all models from `get_available_models` (853 across 9 providers here), filtered as you type; selecting one sends `set_model`; the active model is selected on startup from `get_state`
- Sending while the agent is running queues a **steering message** (delivered after the current turn) instead of erroring
- Preferences window: thinking level (from `get_available_thinking_levels`) and auto-compact toggle — both wired to pi
- Extension UI dialogs (confirm / input / multi-line editor / select / notify toasts); fire-and-forget methods (`setStatus`, …) are ignored without responding
- Stop (abort) button while the agent is running; auto-retry toasts on transient provider errors
- Clear handling when the pi child dies: input disabled and a "pi exited" toast, instead of a silently dead UI

## Build & run

```bash
cd ~/Projects/pi-gui
cargo build          # debug
cargo run            # launch the GUI
cargo build --release
cargo run --release  # launch release build
cargo test           # RPC wire-format serde tests
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
