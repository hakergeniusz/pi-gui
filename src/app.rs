//! Main application window – GNOME HIG / libadwaita styled GUI for pi.
//! Modeled on Claude Desktop / ChatGPT Desktop: sidebar session list, header
//! bar, message bubbles, bottom input area.
//!
//! All RPC traffic flows through the background bridge in `rpc.rs`; this module
//! polls it from the GTK main loop via `glib::timeout_add_local`.

use crate::chat::{self, ChatView, StreamingBubble, ToolBlock};
use crate::rpc::{PiCommand, PiEvent, PiEventStream, PiRpcClient};
use crate::session_list::SessionList;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, DropDown, Entry, EventControllerKey, Image, Label, ListBox, ListBoxRow,
    Orientation, Overlay, PropertyExpression, ScrolledWindow, Separator, Spinner, StringList, Stack,
    TextView, Widget,
};
use libadwaita::prelude::*;
use libadwaita::{
    ComboRow, HeaderBar, MessageDialog, PreferencesGroup, PreferencesPage, PreferencesWindow,
    SwitchRow, TabBar, TabPage, TabView, WindowTitle,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};

type Value = serde_json::Value;

/// One selectable model in the header dropdown.
#[derive(Clone)]
struct ModelInfo {
    label: String, // "provider/modelId"
    provider: String,
    id: String,
}

impl ModelInfo {
    fn new(provider: &str, id: &str) -> Self {
        // Some configured models already carry the provider in their id
        // ("nvidia/nemotron-…"); don't double-prefix those.
        let label = if id.starts_with(&format!("{provider}/")) {
            id.to_string()
        } else {
            format!("{provider}/{id}")
        };
        Self {
            label,
            provider: provider.to_string(),
            id: id.to_string(),
        }
    }
}

/// Main-flow command whose response we are waiting on.
enum Pending {
    None,
    Prompt(Receiver<Value>),
    SwitchSession(Receiver<Value>),
    GetMessages(Receiver<Value>),
}

/// One open chat: its own pi RPC child, transcript, and streaming state.
/// Multiple ChatSessions live in one window as tabs and run concurrently.
struct ChatSession {
    rpc: PiRpcClient,
    events: PiEventStream,

    chat_view: ChatView,
    /// Placeholder shown in the transcript while a resumed session loads.
    loading_row: gtk4::Box,

    streaming: Option<StreamingBubble>,
    tool_blocks: HashMap<String, ToolBlock>,
    /// The current assistant turn already drew its "Agent" header.
    turn_header_shown: bool,
    /// Last known content per file path (from read results), so write/edit
    /// cards can diff against the previous state.
    file_cache: HashMap<String, String>,
    /// toolCallId → (path, kind, content) for read/write calls feeding the
    /// file cache (write carries the new content, applied on success).
    tool_meta: HashMap<String, (String, &'static str, Option<String>)>,

    pending: Pending,
    pending_models: Option<Receiver<Value>>,
    pending_state: Option<(Receiver<Value>, bool)>,
    pending_levels: Option<Receiver<Value>>,

    /// The agent is running a prompt (input still accepts steering messages).
    busy: bool,
    /// The current session already has a display name (don't auto-rename).
    session_named: bool,

    current_session_file: Option<String>,

    suppress_autocompact: Rc<Cell<bool>>,

    /// When the current session switch started (for stderr timing logs).
    switch_started: Option<std::time::Instant>,

    thinking_levels: Vec<String>,
    current_thinking_level: Option<String>,

    /// The tab page this chat renders into.
    page: TabPage,
}

impl ChatSession {
    fn new(tabs: &TabView) -> Self {
        let (rpc, events) = crate::rpc::spawn().expect("failed to start pi RPC");

        let chat_view = ChatView::new();
        let loading_spinner = Spinner::new();
        loading_spinner.start();
        let loading_label = Label::new(Some("Loading messages…"));
        loading_label.add_css_class("loading-label");
        let loading_inner = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(10)
            .halign(Align::Center)
            .valign(Align::Start)
            .margin_top(24)
            .build();
        loading_inner.append(&loading_spinner);
        loading_inner.append(&loading_label);
        let loading_row = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        loading_row.append(&loading_inner);
        loading_row.set_visible(false);

        // The tab shows just the transcript (with the empty-state greeting
        // while there are no messages); the input bar below is shared and acts
        // on the selected tab.
        let page = tabs.append(&chat_view.scrolled);
        page.set_title("New chat");

        Self {
            rpc,
            events,
            chat_view,
            loading_row,
            streaming: None,
            tool_blocks: HashMap::new(),
            turn_header_shown: false,
            file_cache: HashMap::new(),
            tool_meta: HashMap::new(),
            pending: Pending::None,
            pending_models: None,
            pending_state: None,
            pending_levels: None,
            busy: false,
            session_named: false,
            current_session_file: None,
            suppress_autocompact: Rc::new(Cell::new(false)),
            switch_started: None,
            thinking_levels: Vec::new(),
            current_thinking_level: None,
            page,
        }
    }
}

struct State {
    window: gtk4::ApplicationWindow,
    /// The per-session tabs; each tab owns a ChatSession with its own pi child.
    tabs: TabView,
    /// Open chats keyed by an internal id (stable across page lookups).
    chats: HashMap<String, ChatSession>,
    /// Insertion order of chat ids, for event polling.
    chat_order: Vec<String>,
    /// The chat the input area and header act on.
    active: Option<String>,

    stack: Stack,
    session_list: SessionList,
    /// Window-level session discovery: (file path, display name, time label).
    sessions: Vec<(String, String, String)>,

    window_title: WindowTitle,
    busy_spinner: Spinner,
    model_dropdown: DropDown,
    all_models: Vec<ModelInfo>,
    filtered_models: Vec<ModelInfo>,
    current_model_label: Option<String>,
    /// The model selection is applied to the active chat's pi child.
    suppress_model_signal: Rc<Cell<bool>>,

    input_view: TextView,
    input_buffer: gtk4::TextBuffer,
    input_placeholder: Label,
    send_button: Button,
    stop_button: Button,

    suppress_thinking_signal: Rc<Cell<bool>>,
    thinking_combo_row: ComboRow,
    autoswitch_row: SwitchRow,
}

pub struct PiWindow {
    inner: Rc<RefCell<State>>,
}

impl PiWindow {
    pub fn new(app: &libadwaita::Application) -> Self {
        let window = gtk4::ApplicationWindow::builder()
            .application(app)
            .default_width(1100)
            .default_height(760)
            .build();

        // ================= Header bar =================
        let header = HeaderBar::new();

        let window_title = WindowTitle::new("pi", "coding agent");

        // New chat button (start of header)
        let new_chat_btn = Button::builder()
            .icon_name("document-new-symbolic")
            .css_classes(["flat"])
            .valign(Align::Center)
            .build();
        new_chat_btn.set_tooltip_text(Some("New chat"));
        header.pack_start(&new_chat_btn);

        // Additional windows each carry their own independent session.
        let new_window_btn = Button::builder()
            .icon_name("window-new-symbolic")
            .css_classes(["flat"])
            .valign(Align::Center)
            .build();
        new_window_btn.set_tooltip_text(Some("New window — run another session alongside"));
        {
            let app_for_window = app.clone();
            new_window_btn.connect_clicked(move |_| {
                app_for_window.activate();
            });
        }
        header.pack_start(&new_window_btn);

        // Title with a working indicator spinner that appears while the
        // agent runs.
        let busy_spinner = Spinner::new();
        busy_spinner.set_size_request(16, 16);
        busy_spinner.set_visible(false);
        let title_box = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .valign(Align::Center)
            .build();
        title_box.append(&busy_spinner);
        title_box.append(&window_title);

        // pi's first RPC takes seconds (provider + extension discovery); say so.
        window_title.set_subtitle("Starting pi…");
        header.set_title_widget(Some(&title_box));

        // Model dropdown (end): searchable in-place over all providers.
        let model_list = StringList::new(&["Loading models…"]);
        let model_dropdown = DropDown::builder()
            .model(&model_list)
            .hexpand(false)
            .valign(Align::Center)
            .build();
        model_dropdown.set_tooltip_text(Some("Model"));
        model_dropdown.set_enable_search(true);
        let expression = PropertyExpression::new(
            gtk4::StringObject::static_type(),
            None::<&gtk4::Expression>,
            "string",
        );
        model_dropdown.set_expression(Some(&expression));
        header.pack_end(&model_dropdown);

        // Settings button (end)
        let settings_button = Button::builder()
            .icon_name("emblem-system-symbolic")
            .css_classes(["flat"])
            .valign(Align::Center)
            .build();
        settings_button.set_tooltip_text(Some("Preferences"));
        header.pack_end(&settings_button);

        window.set_titlebar(Some(&header));

        // ================= Preferences window =================
        let prefs_window = PreferencesWindow::builder()
            .transient_for(&window)
            .build();
        let prefs_page = PreferencesPage::new();
        prefs_window.add(&prefs_page);

        let behavior_group = PreferencesGroup::new();
        behavior_group.set_title("Behavior");
        prefs_page.add(&behavior_group);

        let thinking_combo_row = ComboRow::builder()
            .title("Thinking level")
            .build();
        behavior_group.add(&thinking_combo_row);

        let autoswitch_row = SwitchRow::builder()
            .title("Auto-compact long sessions")
            .build();
        behavior_group.add(&autoswitch_row);

        let prefs_window_w = prefs_window.clone();
        let window_w = window.clone();
        settings_button.connect_clicked(move |_| {
            prefs_window_w.set_transient_for(Some(&window_w));
            prefs_window_w.present();
        });

        // ================= Main layout: sidebar + stack =================
        let main_box = Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .vexpand(true)
            .build();

        // Sidebar
        let sidebar_scrolled = ScrolledWindow::builder()
            .hexpand(false)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .width_request(280)
            .build();
        sidebar_scrolled.add_css_class("sidebar");

        let session_list = SessionList::new();

        let new_chat_row = Button::builder()
            .hexpand(true)
            .halign(Align::Fill)
            .css_classes(["suggested-action", "pill", "new-chat-row"])
            .build();
        new_chat_row.set_child(Some(&icon_label_box(
            "document-new-symbolic",
            "New chat",
        )));

        let sidebar_inner = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();
        sidebar_inner.append(&new_chat_row);
        sidebar_inner.append(&session_list.scrolled);
        sidebar_scrolled.set_child(Some(&sidebar_inner));

        let separator = Separator::builder()
            .orientation(gtk4::Orientation::Vertical)
            .css_classes(["sidebar-separator"])
            .build();

        // Right stack (chat + welcome)
        let stack = Stack::builder()
            .hexpand(true)
            .vexpand(true)
            .build();

        // Chat page: tab bar + per-session transcripts, shared input below.
        let chat_page = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["chat-page"])
            .build();

        let tabs = TabView::new();
        let tab_bar = TabBar::new();
        tab_bar.set_view(Some(&tabs));
        tab_bar.set_hexpand(true);
        // Keep the bar visible even with a single tab so the multi-session
        // tabs stay discoverable.
        tab_bar.set_autohide(false);
        chat_page.append(&tab_bar);
        chat_page.append(&tabs);

        // Input area: rounded card with a multiline entry, circular send and
        // stop buttons. Enter sends, Shift+Enter inserts a newline.
        let input_card = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .css_classes(["input-card"])
            .build();

        let input_scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(false)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(180)
            .build();

        let input_view = TextView::builder()
            .wrap_mode(gtk4::WrapMode::WordChar)
            .css_classes(["input-textview"])
            .top_margin(11)
            .bottom_margin(11)
            .build();
        let input_buffer = input_view.buffer();

        input_scrolled.set_child(Some(&input_view));

        // TextView has no placeholder support: overlay a dim label shown while
        // the buffer is empty.
        let input_overlay = Overlay::builder().child(&input_scrolled).build();
        let input_placeholder = Label::builder()
            .label("Ask pi to write, explain, or fix code…")
            .halign(Align::Start)
            .valign(Align::Start)
            .margin_top(11)
            .margin_start(2)
            .build();
        input_placeholder.add_css_class("input-placeholder");
        input_overlay.add_overlay(&input_placeholder);

        let send_button = Button::builder()
            .icon_name("document-send-symbolic")
            .css_classes(["circular", "suggested-action"])
            .valign(Align::Center)
            .build();
        send_button.set_sensitive(false);
        send_button.set_tooltip_text(Some("Send"));

        let stop_button = Button::builder()
            .icon_name("process-stop-symbolic")
            .css_classes(["circular", "destructive-action"])
            .valign(Align::Center)
            .build();
        stop_button.set_sensitive(false);
        stop_button.set_tooltip_text(Some("Stop"));

        input_card.append(&input_overlay);
        input_card.append(&stop_button);
        input_card.append(&send_button);
        // The input card tracks the conversation column: same clamp, so both
        // tighten and center together.
        let input_clamp = libadwaita::Clamp::builder()
            .maximum_size(chat::COLUMN_MAX_WIDTH)
            .tightening_threshold(chat::COLUMN_MAX_WIDTH)
            .child(&input_card)
            .build();
        chat_page.append(&input_clamp);

        stack.add_titled(&chat_page, Some("chat"), "Chat");

        // Welcome page (initial view).
        let (welcome_page, welcome_new_chat) = build_welcome_page();
        stack.add_titled(&welcome_page, Some("welcome"), "Welcome");
        stack.set_visible_child_name("welcome");

        main_box.append(&sidebar_scrolled);
        main_box.append(&separator);
        main_box.append(&stack);

        window.set_child(Some(&main_box));

        // ================= State =================
        let state = Rc::new(RefCell::new(State {
            window,
            tabs,
            chats: HashMap::new(),
            chat_order: Vec::new(),
            active: None,
            stack,
            session_list,
            sessions: Vec::new(),
            window_title,
            busy_spinner,
            model_dropdown,
            all_models: Vec::new(),
            filtered_models: Vec::new(),
            current_model_label: None,
            suppress_model_signal: Rc::new(Cell::new(false)),
            input_view,
            input_buffer,
            input_placeholder,
            send_button,
            stop_button,
            suppress_thinking_signal: Rc::new(Cell::new(false)),
            thinking_combo_row,
            autoswitch_row,
        }));

        // ================= Signal handlers =================

        let s = state.clone();
        welcome_new_chat.connect_clicked(move |_| s.borrow_mut().on_new_chat());

        let s = state.clone();
        new_chat_btn.connect_clicked(move |_| s.borrow_mut().on_new_chat());

        let s = state.clone();
        new_chat_row.connect_clicked(move |_| s.borrow_mut().on_new_chat());

        let s = state.clone();
        let entry_view = s.borrow().input_view.clone();
        let key = EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, modifier| {
            let is_enter =
                keyval == gtk4::gdk::Key::Return || keyval == gtk4::gdk::Key::KP_Enter;
            if is_enter && !modifier.contains(gtk4::gdk::ModifierType::SHIFT_MASK) {
                if let Ok(mut st) = s.try_borrow_mut() {
                    st.on_send_message();
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        entry_view.add_controller(key);

        // Send button sensitivity and placeholder follow the buffer content.
        let s = state.clone();
        let send = s.borrow().send_button.clone();
        let placeholder = s.borrow().input_placeholder.clone();
        s.borrow()
            .input_buffer
            .connect_changed(move |buffer| {
                let empty = buffer.char_count() == 0;
                send.set_sensitive(!empty);
                placeholder.set_visible(empty);
            });

        let s = state.clone();
        let suppress = s.borrow().suppress_model_signal.clone();
        let model_dropdown = s.borrow().model_dropdown.clone();
        model_dropdown.connect_selected_notify(move |dd| {
            if suppress.get() {
                return;
            }
            let idx = dd.selected() as usize;
            if let Ok(st) = s.try_borrow() {
                if let Some(info) = st.filtered_models.get(idx).cloned() {
                    drop(st);
                    if let Ok(mut st) = s.try_borrow_mut() {
                        st.set_model(info);
                    }
                }
            }
        });

        let s = state.clone();
        let send_button = s.borrow().send_button.clone();
        send_button.connect_clicked(move |_| s.borrow_mut().on_send_message());

        let s = state.clone();
        let stop_button = s.borrow().stop_button.clone();
        stop_button.connect_clicked(move |_| s.borrow_mut().on_stop());

        let s = state.clone();
        let cb = Rc::new(move |id: &str| {
            s.borrow_mut().open_session_tab(id);
        });
        state.borrow().session_list.set_on_selected(cb);

        let s = state.clone();
        let suppress = s.borrow().suppress_thinking_signal.clone();
        let combo = s.borrow().thinking_combo_row.clone();
        combo.connect_selected_notify(move |row| {
            if suppress.get() {
                return;
            }
            let idx = row.selected() as usize;
            if let Ok(mut st) = s.try_borrow_mut() {
                let level = st
                    .active_chat()
                    .and_then(|c| c.thinking_levels.get(idx).cloned());
                if let Some(level) = level {
                    st.set_thinking_level(level);
                }
            }
        });

        let s = state.clone();
        let switch = s.borrow().autoswitch_row.clone();
        switch.connect_active_notify(move |row| {
            let enabled = row.is_active();
            if let Ok(mut st) = s.try_borrow_mut() {
                // Programmatic row updates happen while the active chat's
                // suppress flag is up; only forward user changes.
                if st.active_chat().is_some_and(|c| c.suppress_autocompact.get()) {
                    return;
                }
                if let Some(chat) = st.active_chat() {
                    let _ = chat.rpc.send(PiCommand::SetAutoCompaction { enabled });
                }
            }
        });

        // Tab lifecycle: closing a tab kills that chat's pi child; selection
        // switches the input area and header to the chosen chat.
        {
            let st = state.clone();
            state.borrow().tabs.connect_close_page(move |view, page| {
                if let Ok(mut s) = st.try_borrow_mut() {
                    s.close_chat(page);
                }
                view.close_page_finish(page, true);
                glib::Propagation::Stop
            });
        }
        {
            let st = state.clone();
            state.borrow().tabs.connect_selected_page_notify(move |_| {
                if let Ok(mut s) = st.try_borrow_mut() {
                    s.sync_active_from_tabs();
                }
            });
        }

        // ================= Kick off =================

        {
            let mut st = state.borrow_mut();
            st.load_disk_sessions();
            // Open the env-specified session(s) — a ';'-separated list opens
            // one tab per session (tests use this to get several tabs without
            // clicks); with none set the first tab shows the pi child's
            // current session.
            let targets: Vec<String> = std::env::var("PI_GUI_OPEN_SESSION")
                .map(|v| {
                    v.split(';')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let known: Vec<String> = targets
                .into_iter()
                .filter(|t| st.sessions.iter().any(|(path, _, _)| path == t))
                .collect();
            if known.is_empty() {
                st.create_chat_tab(None);
            } else {
                for target in &known {
                    st.create_chat_tab(Some(target.as_str()));
                }
            }
        }

        // Main-loop polling of RPC events and command responses. This is the
        // single pump that drives all UI updates.
        let s = state.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if let Ok(mut st) = s.try_borrow_mut() {
                st.poll_events();
            }
            glib::ControlFlow::Continue
        });

        Self { inner: state }
    }

    pub fn present(&self) {
        self.inner.borrow().window.present();
    }
}

// ================= content helpers =================

/// Extract the joined `text` parts of a message `content` value, which is
/// either a plain string or an array of parts
/// (`[{"type":"text","text":"..."}]`).
fn content_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(parts) = content.as_array() else { return String::new() };
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// Extract the joined `thinking` parts of a message `content` value.
fn content_thinking(content: &Value) -> String {
    let Some(parts) = content.as_array() else { return String::new() };
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(|t| t.as_str()) == Some("thinking") {
            if let Some(text) = part.get("thinking").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

fn message_role(message: &Value) -> Option<&str> {
    message.get("role").and_then(|r| r.as_str())
}

/// Format a message's epoch-ms `timestamp` field as a local "HH:MM" label.
fn message_time(message: &Value) -> Option<String> {
    let millis = message.get("timestamp").and_then(|t| t.as_u64())?;
    let seconds = (millis / 1000) as i64;
    let datetime = glib::DateTime::from_unix_local(seconds).ok()?;
    datetime.format("%H:%M").ok().map(|s| s.to_string())
}

/// Short one-line summary of tool-call args for a tool card header.
fn tool_summary(args: Option<&Value>) -> String {
    let Some(obj) = args.and_then(|a| a.as_object()) else { return String::new() };
    for key in ["command", "path", "pattern", "file_path", "query", "url", "name"] {
        if let Some(value) = obj.get(key).and_then(|v| v.as_str()) {
            return shorten_for_display(value, 60);
        }
    }
    // Fall back to the first string-valued argument.
    obj.values()
        .find_map(|v| v.as_str())
        .map(|v| shorten_for_display(v, 60))
        .unwrap_or_default()
}

/// Extract text from a tool result / partial result value
/// (`{"content":[{"type":"text","text":"..."}], ...}`).
fn result_text(result: &Value) -> String {
    result
        .get("content")
        .map(content_text)
        .unwrap_or_default()
}

/// Put the previous→next change into a write/edit tool card body: a git-style
/// diff against the cached file state when one is known, else the plain
/// content. Edit hunks get real line numbers when the file was read earlier.
fn embed_tool_content(
    block: &chat::ToolBlock,
    tool_name: &str,
    args: Option<&Value>,
    chat: &ChatSession,
) {
    let Some(args) = args else { return };
    let name = tool_name.to_lowercase();
    if name.contains("write") {
        let Some(content) = args.get("content").and_then(|c| c.as_str()) else { return };
        let old = args
            .get("path")
            .and_then(|p| p.as_str())
            .and_then(|path| chat.file_cache.get(path).cloned());
        let rows = old.and_then(|old| chat::diff_numbered(&old, content, 1, 1));
        match rows {
            Some(rows) => block.set_embedded_diff(&rows),
            None => block.set_embedded_text(content),
        }
    } else if name.contains("edit") {
        let Some(edits) = args.get("edits").and_then(|e| e.as_array()) else { return };
        let file = args
            .get("path")
            .and_then(|p| p.as_str())
            .and_then(|path| chat.file_cache.get(path).cloned());
        let mut rows: Vec<chat::DiffLine> = Vec::new();
        for edit in edits {
            let Some(old) = edit.get("oldText").and_then(|t| t.as_str()) else { continue };
            let Some(new) = edit.get("newText").and_then(|t| t.as_str()) else { continue };
            // Number from the block's real position in the file when known.
            let start = file
                .as_deref()
                .and_then(|f| chat::find_block_line(f, old))
                .unwrap_or(1);
            let hunk = chat::diff_numbered(old, new, start, start).unwrap_or_else(|| {
                // Fully replaced block: still show it as -old/+new.
                let old = chat::cap_lines(old, 400);
                let new = chat::cap_lines(new, 400);
                let mut rows: Vec<chat::DiffLine> = old
                    .lines()
                    .enumerate()
                    .map(|(i, line)| chat::DiffLine {
                        kind: chat::DiffKind::Old,
                        old_no: Some(start + i as u32),
                        new_no: None,
                        text: line.to_string(),
                    })
                    .collect();
                rows.extend(new.lines().enumerate().map(|(i, line)| chat::DiffLine {
                    kind: chat::DiffKind::New,
                    old_no: None,
                    new_no: Some(start + i as u32),
                    text: line.to_string(),
                }));
                rows
            });
            if !rows.is_empty() {
                rows.push(chat::DiffLine {
                    kind: chat::DiffKind::Context,
                    old_no: None,
                    new_no: None,
                    text: "···".into(),
                });
            }
            rows.extend(hunk);
        }
        block.set_embedded_diff(&rows);
    }
}

/// Shorten a string to `max` chars (on the first line) for display.
fn shorten_for_display(text: &str, max: usize) -> String {
    let first_line = text.lines().next().unwrap_or("").trim();
    let mut chars = first_line.chars();
    let mut out = String::new();
    for _ in 0..max {
        match chars.next() {
            Some(c) => out.push(c),
            None => break,
        }
    }
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

// ================= state actions =================

impl State {
    // ================= chat tab management =================

    fn active_chat(&mut self) -> Option<&mut ChatSession> {
        let id = self.active.clone()?;
        self.chats.get_mut(&id)
    }

    /// Open a new chat tab with its own pi child. `session_path` resumes that
    /// session right away; None starts a fresh chat.
    fn create_chat_tab(&mut self, session_path: Option<&str>) {
        let chat = ChatSession::new(&self.tabs);
        let id = format!("chat-{:03}", self.chat_order.len());
        chat.page.set_loading(true);
        self.tabs.set_selected_page(&chat.page);
        self.chats.insert(id.clone(), chat);
        self.chat_order.push(id.clone());
        self.active = Some(id.clone());
        self.stack.set_visible_child_name("chat");

        // Load this child's state, models and thinking levels.
        let chat = self.chats.get_mut(&id).expect("just inserted");
        chat.pending_state = Some((chat.rpc.send(PiCommand::GetState), false));
        chat.pending_models = Some(chat.rpc.send(PiCommand::GetAvailableModels));
        chat.pending_levels = Some(chat.rpc.send(PiCommand::GetAvailableThinkingLevels));

        if let Some(path) = session_path {
            self.switch_active_chat_to(path);
        }
        self.refresh_chat_ui();
        self.input_view.grab_focus();
    }

    /// Point the active (fresh) chat at an existing session file.
    fn switch_active_chat_to(&mut self, id: &str) {
        let Some(name) =
            self.sessions.iter().find(|(f, _, _)| *f == id).map(|(_, n, _)| n.clone())
        else {
            return;
        };
        let Some(chat) = self.active_chat() else { return };
        chat.current_session_file = Some(id.to_string());
        chat.session_named = true;
        chat.page.set_title(&name);
        chat.chat_view.clear();
        chat.chat_view.append(&chat.loading_row);
        chat.loading_row.set_visible(true);
        chat.page.set_loading(true);
        chat.switch_started = Some(std::time::Instant::now());
        let receiver = chat.rpc.send(PiCommand::SwitchSession { session_path: id.to_string() });
        chat.pending = Pending::SwitchSession(receiver);
    }

    /// Session list click: focus the tab already showing that session, or
    /// open it in a new tab. Running chats are never interrupted.
    fn open_session_tab(&mut self, id: &str) {
        let open_chat = self
            .chats
            .iter()
            .find(|(_, c)| c.current_session_file.as_deref() == Some(id))
            .map(|(cid, _)| cid.clone());
        if let Some(cid) = open_chat {
            if let Some(chat) = self.chats.get(&cid) {
                self.tabs.set_selected_page(&chat.page);
            }
            self.stack.set_visible_child_name("chat");
            return;
        }
        // A fresh chat with no history yet can adopt the session in place.
        let empty_active = self
            .active_chat()
            .is_some_and(|c| c.current_session_file.is_none() && !c.busy);
        self.stack.set_visible_child_name("chat");
        if empty_active {
            self.switch_active_chat_to(id);
            self.refresh_chat_ui();
        } else {
            self.create_chat_tab(Some(id));
        }
    }

    fn close_chat(&mut self, page: &TabPage) {
        let Some(id) = self
            .chats
            .iter()
            .find(|(_, c)| c.page == *page)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        // Dropping the ChatSession closes the RPC channel, which exits the pi
        // child.
        self.chats.remove(&id);
        self.chat_order.retain(|c| *c != id);
        if self.active.as_deref() == Some(id.as_str()) {
            self.active = self.chat_order.last().cloned();
        }
        if self.chats.is_empty() {
            self.stack.set_visible_child_name("welcome");
        }
        self.refresh_chat_ui();
    }

    /// Re-read the selected tab and sync the shared UI to it.
    fn sync_active_from_tabs(&mut self) {
        let selected = self.tabs.selected_page();
        let id = self
            .chats
            .iter()
            .find(|(_, c)| Some(&c.page) == selected.as_ref())
            .map(|(id, _)| id.clone());
        self.active = id.or_else(|| self.chat_order.last().cloned());
        self.refresh_chat_ui();
        self.input_view.grab_focus();
    }

    /// Push the active chat's state into the shared widgets (title, spinner,
    /// stop button, sidebar highlight).
    fn refresh_chat_ui(&mut self) {
        let active = self.active.clone().and_then(|id| self.chats.get(&id));
        match active {
            Some(chat) => {
                self.window_title.set_title(&chat.page.title());
                self.window_title.set_subtitle(if chat.busy {
                    "Working…"
                } else {
                    "Idle"
                });
                self.stop_button.set_sensitive(chat.busy);
                if let Some(file) = &chat.current_session_file {
                    self.session_list.set_active(file);
                }
            }
            None => {
                self.window_title.set_title("pi");
                self.window_title.set_subtitle("GNOME AI coding assistant");
                self.stop_button.set_sensitive(false);
            }
        }
        let any_busy = self.chats.values().any(|c| c.busy);
        if any_busy {
            self.busy_spinner.start();
            self.busy_spinner.set_visible(true);
        } else {
            self.busy_spinner.stop();
            self.busy_spinner.set_visible(false);
        }
    }

    // ================= actions =================

    fn on_new_chat(&mut self) {
        self.create_chat_tab(None);
    }

    fn input_text(&self) -> String {
        let (start, end) = self.input_buffer.bounds();
        self.input_buffer.text(&start, &end, false).to_string()
    }

    fn on_send_message(&mut self) {
        let text = self.input_text();
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        let Some(id) = self.active.clone() else { return };
        self.input_buffer.set_text("");
        self.stack.set_visible_child_name("chat");

        // User bubble (in the active chat's transcript).
        let buffer = self.input_buffer.clone();
        let view = self.input_view.clone();
        let sent = trimmed.clone();
        let on_edit: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
            buffer.set_text(&sent);
            view.grab_focus();
        });

        let session_named_info = {
            let Some(chat) = self.chats.get_mut(&id) else { return };
            let time = chat::now_time();
            let bubble = chat::user_message(&trimmed, Some(&time), Some(on_edit));
            chat.chat_view.append(&bubble.box_);

            // Name the session after the first user message.
            let mut renamed = None;
            if !chat.session_named {
                if let Some(file) = chat.current_session_file.clone() {
                    let name = shorten_for_display(&trimmed, 48);
                    if !name.is_empty() {
                        let _ = chat.rpc.send(PiCommand::SetSessionName { name: name.clone() });
                        renamed = Some((file, name));
                    }
                    chat.session_named = true;
                }
            }
            renamed
        };

        if let Some((file, name)) = session_named_info {
            self.session_list.rename_session(&file, &name);
            if let Some(entry) = self.sessions.iter_mut().find(|(f, _, _)| *f == file) {
                entry.1 = name.clone();
            }
            if let Some(chat) = self.chats.get(&id) {
                chat.page.set_title(&name);
            }
        }

        let Some(chat) = self.chats.get_mut(&id) else { return };
        if chat.busy {
            // The agent is already running: queue a steering message that is
            // delivered after the current assistant turn finishes its tool
            // calls.
            let _ = chat.rpc.send(PiCommand::Steer { message: trimmed });
            eprintln!("[pi-gui] message queued — sent when the current turn finishes");
        } else {
            // New turn: the response draws one "Agent" header.
            chat.turn_header_shown = false;
            let streaming = StreamingBubble::new(true);
            chat.turn_header_shown = true;
            chat.chat_view.append(&streaming.container);
            chat.streaming = Some(streaming);
            chat.chat_view.scroll_to_bottom();

            chat.busy = true;
            chat.page.set_loading(true);
            let receiver = chat.rpc.send(PiCommand::Prompt { message: trimmed });
            chat.pending = Pending::Prompt(receiver);
        }
        self.refresh_chat_ui();
    }

    fn on_stop(&mut self) {
        if let Some(chat) = self.active_chat() {
            if chat.busy {
                let _ = chat.rpc.send(PiCommand::Abort);
            }
        }
    }

    // ================= event/response polling =================

    fn poll_events(&mut self) {
        // Each chat owns a pi child; pump every one of them. Taking the chat
        // out of the map keeps borrows disjoint while handling events.
        let ids = self.chat_order.clone();
        for id in ids {
            let Some(mut chat) = self.chats.remove(&id) else { continue };
            while let Some(event) = chat.events.try_event() {
                self.handle_event(&id, &mut chat, &event);
            }
            self.poll_chat_pending(&id, &mut chat);
            self.chats.insert(id, chat);
        }
    }

    fn poll_chat_pending(&mut self, id: &str, chat: &mut ChatSession) {
        if let Some(recv) = chat.pending_models.take() {
            match recv.try_recv() {
                Ok(value) => self.load_models_done(id, chat, &value),
                Err(TryRecvError::Empty) => chat.pending_models = Some(recv),
                Err(TryRecvError::Disconnected) => {
                    eprintln!("[pi-gui] model list unavailable (chat {id})")
                }
            }
        }

        if let Some((recv, after_new)) = chat.pending_state.take() {
            match recv.try_recv() {
                Ok(value) => self.load_state_done(id, chat, &value, after_new),
                Err(TryRecvError::Empty) => chat.pending_state = Some((recv, after_new)),
                Err(TryRecvError::Disconnected) => {
                    eprintln!("[pi-gui] state sync failed (chat {id})")
                }
            }
        }

        if let Some(recv) = chat.pending_levels.take() {
            match recv.try_recv() {
                Ok(value) => self.load_levels_done(id, chat, &value),
                Err(TryRecvError::Empty) => chat.pending_levels = Some(recv),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        match std::mem::replace(&mut chat.pending, Pending::None) {
            Pending::None => {}
            Pending::Prompt(recv) => match recv.try_recv() {
                Ok(value) => {
                    let ok = value.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                    if !ok {
                        let err = value
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown error");
                        eprintln!("[pi-gui] prompt failed: {err}");
                        chat.busy = false;
                        chat.page.set_loading(false);
                        self.finalize_streaming(chat, None, None);
                        self.refresh_chat_ui();
                    }
                }
                Err(TryRecvError::Empty) => chat.pending = Pending::Prompt(recv),
                Err(TryRecvError::Disconnected) => {
                    eprintln!("[pi-gui] prompt failed — RPC bridge died");
                    chat.busy = false;
                    chat.page.set_loading(false);
                    self.finalize_streaming(chat, None, None);
                    self.refresh_chat_ui();
                }
            },
            Pending::SwitchSession(recv) => match recv.try_recv() {
                Ok(value) => {
                    let ok = value.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                    let cancelled = value
                        .pointer("/data/cancelled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if ok && !cancelled {
                        chat.pending = Pending::GetMessages(chat.rpc.send(PiCommand::GetMessages));
                    } else {
                        chat.loading_row.set_visible(false);
                        chat.chat_view.remove_row(&chat.loading_row);
                        self.refresh_chat_ui();
                    }
                }
                Err(TryRecvError::Empty) => chat.pending = Pending::SwitchSession(recv),
                Err(TryRecvError::Disconnected) => {
                    chat.loading_row.set_visible(false);
                    chat.chat_view.remove_row(&chat.loading_row);
                    eprintln!("[pi-gui] session switch failed — RPC bridge died");
                }
            },
            Pending::GetMessages(recv) => match recv.try_recv() {
                Ok(value) => self.load_history_done(chat, &value),
                Err(TryRecvError::Empty) => chat.pending = Pending::GetMessages(recv),
                Err(TryRecvError::Disconnected) => {}
            },
        }
    }

    fn load_history_done(&mut self, chat: &mut ChatSession, value: &Value) {
        chat.loading_row.set_visible(false);
        chat.chat_view.remove_row(&chat.loading_row);
        // The switch is complete; a turn that already started keeps the
        // spinner, an idle chat drops it.
        chat.page.set_loading(chat.busy);
        if let Some(started) = chat.switch_started.take() {
            let render_start = std::time::Instant::now();
            let count = value
                .pointer("/data/messages")
                .and_then(|m| m.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            if let Some(messages) = value.pointer("/data/messages") {
                self.render_messages(chat, messages);
            }
            chat.chat_view.scroll_to_bottom();
            eprintln!(
                "[timing] session load: {} ms to messages ({}), {} ms to render",
                started.elapsed().as_millis(),
                count,
                render_start.elapsed().as_millis()
            );
        }
        self.refresh_chat_ui();
    }

    fn render_messages(&self, chat: &mut ChatSession, messages: &Value) {
        let Some(arr) = messages.as_array() else { return };
        // One "Agent" header per assistant run: it appears on the first
        // assistant message after anything that is not an assistant message.
        let mut prev_role: Option<String> = None;
        for msg in arr {
            let content = msg.get("content").cloned().unwrap_or_default();
            let text = content_text(&content);
            let thinking = content_thinking(&content);
            let time = message_time(msg);
            let time = time.as_deref();
            match message_role(msg) {
                Some("user") if !text.is_empty() => {
                    let buffer = self.input_buffer.clone();
                    let view = self.input_view.clone();
                    let owned = text.clone();
                    let on_edit: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
                        buffer.set_text(&owned);
                        view.grab_focus();
                    });
                    chat.chat_view
                        .append(&chat::user_message(&text, time, Some(on_edit)).box_);
                    prev_role = Some("user".into());
                }
                Some("assistant") if !text.is_empty() || !thinking.is_empty() => {
                    let thinking = if thinking.is_empty() { None } else { Some(thinking.as_str()) };
                    let with_header = prev_role.as_deref() != Some("assistant");
                    chat.chat_view.append(
                        &chat::assistant_message(&text, thinking, time, with_header, None).box_,
                    );
                    prev_role = Some("assistant".into());
                }
                _ => {}
            }
        }
    }

    // ================= model list =================

    fn load_models_done(&mut self, id: &str, _chat: &mut ChatSession, value: &Value) {
        let mut models = Vec::new();
        if let Some(arr) = value.pointer("/data/models").and_then(|m| m.as_array()) {
            for m in arr {
                let (Some(provider), Some(id)) = (
                    m.get("provider").and_then(|p| p.as_str()),
                    m.get("id").and_then(|i| i.as_str()),
                ) else {
                    continue;
                };
                models.push(ModelInfo::new(provider, id));
            }
        }
        if models.is_empty() {
            models = models_from_config_file();
        }
        if models.is_empty() {
            eprintln!("[pi-gui] no models available (chat {id})");
            return;
        }
        models.sort_by(|a, b| a.label.cmp(&b.label));
        // The dropdown is shared; only fill it once (models are global).
        if self.all_models.is_empty() {
            self.all_models = models;
            self.filtered_models = self.all_models.clone();

            let string_list = StringList::new(&[]);
            for m in &self.filtered_models {
                string_list.append(&m.label);
            }
            self.suppress_model_signal.set(true);
            self.model_dropdown.set_model(Some(&string_list));
            if let Some(label) = self.current_model_label.clone() {
                if let Some(idx) = self.filtered_models.iter().position(|m| m.label == label) {
                    self.model_dropdown.set_selected(idx as u32);
                }
            }
            self.suppress_model_signal.set(false);
        }
    }

    fn select_current_model(&mut self) {
        let Some(label) = self.current_model_label.clone() else { return };
        if let Some(idx) = self.filtered_models.iter().position(|m| m.label == label) {
            self.suppress_model_signal.set(true);
            self.model_dropdown.set_selected(idx as u32);
            self.suppress_model_signal.set(false);
        }
    }

    fn set_model(&mut self, info: ModelInfo) {
        self.current_model_label = Some(info.label.clone());
        if let Some(chat) = self.active_chat() {
            let _ = chat.rpc.send(PiCommand::SetModel { provider: info.provider, model_id: info.id });
        }
    }

    // ================= state sync =================

    fn load_state_done(&mut self, id: &str, chat: &mut ChatSession, value: &Value, after_new: bool) {
        let empty = serde_json::Value::Null;
        let data = value.get("data").unwrap_or(&empty);

        // Model.
        if let Some(model) = data.get("model") {
            if let (Some(provider), Some(model_id)) = (
                model.get("provider").and_then(|p| p.as_str()),
                model.get("id").and_then(|i| i.as_str()),
            ) {
                let label = ModelInfo::new(provider, model_id).label;
                // The dropdown reflects whichever chat is selected; the first
                // state load wins (models are global anyway).
                if self.current_model_label.is_none() || self.active.as_deref() == Some(id) {
                    self.current_model_label = Some(label);
                    self.select_current_model();
                }
            }
        }

        // Thinking level.
        if let Some(level) = data.get("thinkingLevel").and_then(|l| l.as_str()) {
            chat.current_thinking_level = Some(level.to_string());
            if self.active.as_deref() == Some(id) {
                self.select_thinking_level(chat, level);
            }
        }

        // Session.
        if let Some(file) = data.get("sessionFile").and_then(|f| f.as_str()) {
            let file = file.to_string();
            let name = data
                .get("sessionName")
                .and_then(|n| n.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    if after_new { "New chat".to_string() } else { "Current session".to_string() }
                });
            let time_label = time_label_now();
            if !self.sessions.iter().any(|(f, _, _)| *f == file) {
                self.sessions.push((file.clone(), name.clone(), time_label.clone()));
            } else if let Some(entry) = self.sessions.iter_mut().find(|(f, _, _)| *f == file) {
                entry.1 = name.clone();
            }
            self.session_list.add_session(&file, &name, &time_label, true);
            chat.current_session_file = Some(file.clone());
            // Keep the default tab title for fresh chats; resumed ones get
            // their real name when switched to.
            if !chat.session_named {
                chat.page.set_title(&name);
            }
            self.session_list.set_active(&file);
        }

        // Auto-compaction (the row reflects the active chat's child).
        if self.active.as_deref() == Some(id) {
            if let Some(enabled) = data.get("autoCompactionEnabled").and_then(|v| v.as_bool()) {
                chat.suppress_autocompact.set(true);
                self.autoswitch_row.set_active(enabled);
                chat.suppress_autocompact.set(false);
            }
        }

        // Thinking levels for the current model.
        chat.pending_levels = Some(chat.rpc.send(PiCommand::GetAvailableThinkingLevels));
        // A fresh chat never runs a switch/turn; drop its startup spinner.
        chat.page.set_loading(chat.busy);
        self.refresh_chat_ui();
    }

    fn load_levels_done(&mut self, id: &str, chat: &mut ChatSession, value: &Value) {
        let levels: Vec<String> = value
            .pointer("/data/levels")
            .and_then(|l| l.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| l.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if levels.is_empty() {
            return;
        }

        chat.thinking_levels = levels;
        // The combo row is shared; refresh it only for the selected chat.
        if self.active.as_deref() != Some(id) {
            return;
        }
        let string_list = StringList::new(&[]);
        for level in &chat.thinking_levels {
            string_list.append(level);
        }
        self.suppress_thinking_signal.set(true);
        self.thinking_combo_row.set_model(Some(&string_list));
        if let Some(current) = chat.current_thinking_level.clone() {
            if let Some(idx) = chat.thinking_levels.iter().position(|l| *l == current) {
                self.thinking_combo_row.set_selected(idx as u32);
            }
        }
        self.suppress_thinking_signal.set(false);
    }

    fn select_thinking_level(&mut self, chat: &mut ChatSession, level: &str) {
        chat.current_thinking_level = Some(level.to_string());
        if let Some(idx) = chat.thinking_levels.iter().position(|l| l == level) {
            self.suppress_thinking_signal.set(true);
            self.thinking_combo_row.set_selected(idx as u32);
            self.suppress_thinking_signal.set(false);
        }
    }

    fn set_thinking_level(&mut self, level: String) {
        if let Some(chat) = self.active_chat() {
            chat.current_thinking_level = Some(level.clone());
            let _ = chat.rpc.send(PiCommand::SetThinkingLevel { level });
        }
    }

    // ================= session discovery =================

    /// Load recent sessions from pi's session store into the sidebar.
    fn load_disk_sessions(&mut self) {
        let sessions = scan_disk_sessions();
        for (path, name, time) in &sessions {
            self.session_list.add_session(path, name, time, false);
        }
        self.sessions = sessions;
    }

    // ================= agent events =================

    fn handle_event(&mut self, id: &str, chat: &mut ChatSession, event: &PiEvent) {
        // Only the selected chat's state is mirrored into the shared widgets.
        let is_active = self.active.as_deref() == Some(id);
        match event {
            PiEvent::AgentStart => {
                chat.busy = true;
                chat.page.set_loading(true);
                if let Some(file) = chat.current_session_file.clone() {
                    self.session_list.set_busy(&file, true);
                }
                if is_active {
                    self.stop_button.set_sensitive(true);
                }
                self.refresh_chat_ui();
            }
            PiEvent::AgentSettled { aborted: _ } => {
                chat.busy = false;
                chat.page.set_loading(false);
                if let Some(file) = chat.current_session_file.clone() {
                    self.session_list.set_busy(&file, false);
                }
                self.finalize_streaming(chat, None, None);
                chat.chat_view.scroll_to_bottom();
                self.refresh_chat_ui();
            }
            PiEvent::AgentEnd | PiEvent::TurnStart | PiEvent::TurnEnd | PiEvent::Other => {}
            PiEvent::Disconnected => {
                chat.busy = false;
                chat.page.set_loading(false);
                chat.page.set_title(&format!("{} — disconnected", chat.page.title()));
                if let Some(file) = chat.current_session_file.clone() {
                    self.session_list.set_busy(&file, false);
                }
                if is_active {
                    self.stop_button.set_sensitive(false);
                    self.window_title.set_subtitle("pi disconnected — restart pi-gui");
                }
                eprintln!("[pi-gui] chat {id}: pi exited — RPC disconnected");
                self.refresh_chat_ui();
            }
            PiEvent::Response { .. } => {}
            PiEvent::MessageStart { message } => {
                if message.as_ref().and_then(message_role) == Some("assistant") {
                    let with_header = !chat.turn_header_shown;
                    self.ensure_streaming(chat, with_header);
                    if with_header {
                        chat.turn_header_shown = true;
                    }
                }
            }
            PiEvent::MessageUpdate { event } => self.handle_message_update(chat, event.as_ref()),
            PiEvent::MessageEnd { message } => {
                if let Some(message) = message {
                    if message_role(message) == Some("assistant") {
                        let content = message.get("content").cloned().unwrap_or_default();
                        let text = content_text(&content);
                        let thinking = content_thinking(&content);
                        let thinking = if thinking.is_empty() { None } else { Some(thinking) };
                        self.finalize_streaming(chat, Some(&text), thinking.as_deref());
                    }
                }
            }
            PiEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                let name = tool_name.clone().unwrap_or_else(|| "tool".into());
                let summary = tool_summary(args.as_ref());
                let block = ToolBlock::new(&name, &summary);
                embed_tool_content(&block, &name, args.as_ref(), chat);
                chat.chat_view.append(&block.box_);
                if let Some(call_id) = tool_call_id {
                    chat.tool_blocks.insert(call_id.clone(), block);
                    if let Some(path) =
                        args.as_ref().and_then(|a| a.get("path")).and_then(|p| p.as_str())
                    {
                        let lower = name.to_lowercase();
                        let (kind, content) = if lower.contains("read") {
                            ("read", None)
                        } else if lower.contains("write") {
                            (
                                "write",
                                args.as_ref()
                                    .and_then(|a| a.get("content"))
                                    .and_then(|c| c.as_str())
                                    .map(str::to_string),
                            )
                        } else {
                            ("other", None)
                        };
                        chat.tool_meta.insert(call_id.clone(), (path.to_string(), kind, content));
                    }
                }
                chat.chat_view.scroll_to_bottom();
            }
            PiEvent::ToolExecutionUpdate { tool_call_id, partial_result } => {
                let text = partial_result.as_ref().map(result_text).unwrap_or_default();
                if let (Some(call_id), false) = (tool_call_id, text.is_empty()) {
                    if let Some(block) = chat.tool_blocks.get(call_id) {
                        if block.is_embedded() {
                            block.append_result(&text);
                        } else {
                            block.set_content(&text);
                        }
                    }
                }
            }
            PiEvent::ToolExecutionEnd { tool_call_id, result, is_error } => {
                let text = result.as_ref().map(result_text).unwrap_or_default();
                if let Some(call_id) = tool_call_id {
                    if let Some(block) = chat.tool_blocks.get(call_id) {
                        if !text.is_empty() {
                            if block.is_embedded() {
                                block.append_result(&text);
                            } else {
                                block.set_content(&text);
                            }
                        }
                        block.set_finished(*is_error == Some(true));
                    }
                    // A successful read reveals the file's current content and
                    // a successful write sets it — remember both so later
                    // write/edit cards can diff against it.
                    if *is_error != Some(true) {
                        if let Some((path, kind, content)) = chat.tool_meta.get(call_id) {
                            match (*kind, content) {
                                ("read", _) if !text.is_empty() => {
                                    chat.file_cache.insert(path.clone(), text.clone());
                                }
                                ("write", Some(new)) => {
                                    chat.file_cache.insert(path.clone(), new.clone());
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            PiEvent::EntryAppended { .. } => {}
            PiEvent::AutoRetryStart { attempt, error_message } => {
                if let Some(attempt) = attempt {
                    // The streaming bubble carries a persistent status line;
                    // toasts here would just add noise on every attempt.
                    let reason = error_message
                        .as_deref()
                        .map(|s| shorten_for_display(s, 80))
                        .unwrap_or_default();
                    self.ensure_streaming(chat, true);
                    if let Some(streaming) = chat.streaming.as_ref() {
                        streaming.show_retry(*attempt, &reason);
                    }
                    chat.chat_view.scroll_to_bottom();
                }
            }
            PiEvent::AutoRetryEnd { success: Some(true), .. } => {
                if let Some(streaming) = chat.streaming.as_ref() {
                    streaming.retry_done();
                }
            }
            PiEvent::AutoRetryEnd { success: Some(false), final_error, .. } => {
                let err = final_error.clone().unwrap_or_default();
                // Flush any partial streamed content, then leave a permanent
                // error row in the transcript.
                self.finalize_streaming(chat, None, None);
                let err = if err.is_empty() {
                    "The provider failed after repeated retries.".to_string()
                } else {
                    format!("Provider error after retries: {}", shorten_for_display(&err, 200))
                };
                let bubble = chat::error_message(&err);
                chat.chat_view.append(&bubble.box_);
                chat.chat_view.scroll_to_bottom();
            }
            PiEvent::AutoRetryEnd { .. } => {}
            PiEvent::SessionInfoChanged { name } => {
                if let Some(name) = name {
                    chat.page.set_title(name);
                    if let Some(file) = chat.current_session_file.clone() {
                        self.session_list.rename_session(&file, name);
                        if let Some(entry) =
                            self.sessions.iter_mut().find(|(f, _, _)| *f == file)
                        {
                            entry.1 = name.clone();
                        }
                    }
                    if is_active {
                        self.window_title.set_title(name);
                    }
                }
            }
            PiEvent::ThinkingLevelChanged { level } => {
                if let Some(level) = level {
                    chat.current_thinking_level = Some(level.to_string());
                    if is_active {
                        self.select_thinking_level(chat, level);
                    }
                }
            }
            PiEvent::ExtensionUiRequest { id, method, extra } => {
                self.handle_extension_request(id, method, extra, chat);
            }
            PiEvent::ExtensionError { extension_path, error, .. } => {
                eprintln!("[extension error] {extension_path}: {error}");
            }
        }
    }

    fn handle_message_update(&mut self, chat: &mut ChatSession, event: Option<&Value>) {
        let Some(event) = event else { return };
        match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text_start" => {
                self.ensure_streaming(chat, !chat.turn_header_shown);
            }
            "text_delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.ensure_streaming(chat, !chat.turn_header_shown);
                    if let Some(streaming) = chat.streaming.as_ref() {
                        streaming.append_text(delta);
                    }
                    chat.chat_view.scroll_to_bottom();
                }
            }
            "thinking_delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.ensure_streaming(chat, !chat.turn_header_shown);
                    if let Some(streaming) = chat.streaming.as_ref() {
                        streaming.append_thinking(delta);
                    }
                    chat.chat_view.scroll_to_bottom();
                }
            }
            "text_end" => {
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    if let Some(streaming) = chat.streaming.as_ref() {
                        streaming.set_text(content);
                    }
                }
            }
            "thinking_end" => {
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    if let Some(streaming) = chat.streaming.as_ref() {
                        streaming.set_thinking(content);
                        streaming.finish_thinking();
                    }
                }
            }
            "error" => {
                let msg = event.get("error").and_then(|e| e.as_str()).unwrap_or("stream error");
                eprintln!("[pi-gui] stream error: {msg}");
            }
            _ => {}
        }
    }

    /// Make sure a streaming bubble exists, drawing the turn header on its
    /// first row only; remember that the turn's header was drawn.
    fn ensure_streaming(&mut self, chat: &mut ChatSession, with_header: bool) {
        if chat.streaming.is_none() {
            let streaming = StreamingBubble::new(with_header);
            if with_header {
                chat.turn_header_shown = true;
            }
            chat.chat_view.append(&streaming.container);
            chat.streaming = Some(streaming);
            chat.chat_view.scroll_to_bottom();
        }
    }

    /// Replace the streaming bubble with a final assistant bubble. With
    /// `text: None` any dangling streaming bubble is just removed.
    fn finalize_streaming(&mut self, chat: &mut ChatSession, text: Option<&str>, thinking: Option<&str>) {
        if let Some(streaming) = chat.streaming.take() {
            let text = text.unwrap_or("");
            let thinking = thinking.unwrap_or("");
            let has_content = !text.is_empty() || !thinking.is_empty();
            if has_content {
                let bubble = chat::assistant_message(
                    text,
                    if thinking.is_empty() { None } else { Some(thinking) },
                    None,
                    streaming.has_header,
                    streaming.thought_secs(),
                );
                // Insert at the streaming bubble's position so tool blocks
                // that already followed it stay in order.
                chat.chat_view.list.insert_child_after(&bubble.box_, Some(&streaming.container));
            }
            chat.chat_view.remove_row(&streaming.container);
            // The swap changes heights after the last append scrolled; land on
            // the final bubble, not wherever the removed bubble left the view.
            chat.chat_view.scroll_to_bottom();
        }
    }

    // ================= extension UI requests =================

    fn handle_extension_request(&self, id: &str, method: &str, extra: &Value, chat: &ChatSession) {
        match method {
            "confirm" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Confirm");
                let message = extra.get("message").and_then(|m| m.as_str()).unwrap_or("");
                self.show_confirm(id, title, message, chat);
            }
            "input" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Input");
                let placeholder = extra.get("placeholder").and_then(|p| p.as_str()).unwrap_or("");
                self.show_input(id, title, placeholder, chat);
            }
            "editor" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Edit text");
                let prefill = extra.get("prefill").and_then(|p| p.as_str()).unwrap_or("");
                self.show_editor(id, title, prefill, chat);
            }
            "select" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Select");
                self.show_select(id, title, extra, chat);
            }
            // Fire-and-forget methods: log, never respond.
            "notify" => {
                let message = extra.get("message").and_then(|m| m.as_str()).unwrap_or("");
                eprintln!("[extension notify] {message}");
            }
            "setStatus" | "setWidget" | "setTitle" | "set_editor_text" | _ => {}
        }
    }

    fn show_confirm(&self, id: &str, title: &str, message: &str, chat: &ChatSession) {
        let dialog = MessageDialog::builder()
            .heading(title)
            .body(message)
            .transient_for(&self.window)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");

        let id = id.to_string();
        let rpc = chat.rpc.clone();
        dialog.connect_local("response", false, move |values| {
            let response = values
                .first()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default();
            let confirmed = response == "ok";
            rpc.extension_response(&id, serde_json::json!({ "confirmed": confirmed }));
            None
        });
        dialog.present();
    }

    fn show_input(&self, id: &str, title: &str, placeholder: &str, chat: &ChatSession) {
        let dialog = MessageDialog::builder()
            .heading(title)
            .transient_for(&self.window)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");

        let entry = Entry::builder()
            .placeholder_text(placeholder)
            .hexpand(true)
            .build();
        dialog.set_extra_child(Some(&entry));

        let id = id.to_string();
        let rpc = chat.rpc.clone();
        let entry_weak = entry.downgrade();
        dialog.connect_local("response", false, move |values| {
            let response = values
                .first()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default();
            let cancelled = response != "ok";
            let value = entry_weak
                .upgrade()
                .map(|e| e.text().to_string())
                .unwrap_or_default();
            if cancelled {
                rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
            } else {
                rpc.extension_response(&id, serde_json::json!({ "value": value }));
            }
            None
        });
        dialog.present();
    }

    fn show_editor(&self, id: &str, title: &str, prefill: &str, chat: &ChatSession) {
        let dialog = MessageDialog::builder()
            .heading(title)
            .transient_for(&self.window)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .width_request(420)
            .height_request(260)
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .build();
        let view = TextView::builder()
            .hexpand(true)
            .vexpand(true)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .build();
        view.buffer().set_text(prefill);
        scrolled.set_child(Some(&view));
        dialog.set_extra_child(Some(&scrolled));

        let id = id.to_string();
        let rpc = chat.rpc.clone();
        let view_weak = view.downgrade();
        dialog.connect_local("response", false, move |values| {
            let response = values
                .first()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default();
            let cancelled = response != "ok";
            let value = view_weak
                .upgrade()
                .map(|v| {
                    let buffer = v.buffer();
                    let (start, end) = buffer.bounds();
                    buffer.text(&start, &end, false).to_string()
                })
                .unwrap_or_default();
            if cancelled {
                rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
            } else {
                rpc.extension_response(&id, serde_json::json!({ "value": value }));
            }
            None
        });
        dialog.present();
    }

    fn show_select(&self, id: &str, title: &str, extra: &Value, chat: &ChatSession) {
        let options = extra
            .get("options")
            .and_then(|o| o.as_array())
            .cloned()
            .unwrap_or_default();
        let dialog = MessageDialog::builder()
            .heading(title)
            .transient_for(&self.window)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");

        let list = ListBox::builder()
            .selection_mode(gtk4::SelectionMode::Single)
            .hexpand(true)
            .build();
        for opt in options.iter() {
            let label = opt.as_str().unwrap_or("");
            let row = ListBoxRow::builder().hexpand(true).build();
            let lbl = Label::builder()
                .label(label)
                .xalign(0.0)
                .hexpand(true)
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(12)
                .margin_end(12)
                .build();
            row.set_child(Some(&lbl));
            list.append(&row);
        }
        dialog.set_extra_child(Some(&list));

        let id = id.to_string();
        let rpc = chat.rpc.clone();
        let list_weak = list.downgrade();
        dialog.connect_local("response", false, move |values| {
            let response = values
                .first()
                .and_then(|v| v.get::<String>().ok())
                .unwrap_or_default();
            let cancelled = response != "ok";
            if cancelled {
                rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
            } else if let Some(list) = list_weak.upgrade() {
                if let Some(row) = list.selected_row() {
                    if let Some(lbl) = row.first_child().and_then(|c| c.downcast::<Label>().ok()) {
                        rpc.extension_response(
                            &id,
                            serde_json::json!({ "value": lbl.text().to_string() }),
                        );
                    } else {
                        rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
                    }
                } else {
                    rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
                }
            } else {
                rpc.extension_response(&id, serde_json::json!({ "cancelled": true }));
            }
            None
        });
        dialog.present();
    }

}

// ================= session discovery helpers =================

/// Short human label for a session's age: "Today", "Yesterday", or a
/// "Mon D" style date.
fn time_label(mtime: std::time::SystemTime) -> String {
    let datetime = glib::DateTime::now_local().ok();
    let file = glib::DateTime::from_unix_local(
        mtime.duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    )
    .ok();
    let (Some(now), Some(file)) = (datetime, file) else {
        return String::new();
    };
    match now.difference(&file) {
        diff if diff.as_days() == 0 && now.day_of_year() == file.day_of_year() => {
            format!("Today {}", file.format("%H:%M").unwrap_or_default())
        }
        diff if diff.as_days() <= 1 => "Yesterday".to_string(),
        _ => file.format("%b %e").unwrap_or_default().to_string(),
    }
}

/// Time label for a session created right now.
fn time_label_now() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("Today %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Scan pi's session store (`~/.pi/agent/sessions/`) for recent `.jsonl`
/// session files, newest first (max 40).
fn scan_disk_sessions() -> Vec<(String, String, String)> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let root = std::path::Path::new(&home).join(".pi/agent/sessions");
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    collect_session_files(&root, &mut files, 0);
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
        .into_iter()
        .take(40)
        .map(|(path, mtime)| {
            let name = session_display_name(&path);
            let time = time_label(mtime);
            (path.to_string_lossy().into_owned(), name, time)
        })
        .collect()
}

fn collect_session_files(
    dir: &std::path::Path,
    out: &mut Vec<(std::path::PathBuf, std::time::SystemTime)>,
    depth: u32,
) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_session_files(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((path, mtime));
        }
    }
}

/// Derive a sidebar label for a session file: prefer a `session_info` name
/// entry, else the first user message (like pi's own `/resume` picker).
fn session_display_name(path: &std::path::Path) -> String {
    let mut buf = Vec::new();
    if let Ok(mut file) = std::fs::File::open(path) {
        use std::io::Read;
        let _ = file.by_ref().take(64 * 1024).read_to_end(&mut buf);
    }
    let text = String::from_utf8_lossy(&buf);

    let mut name: Option<String> = None;
    let mut first_user: Option<String> = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else { continue };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("session_info") => {
                if let Some(n) = value.get("name").and_then(|n| n.as_str()) {
                    name = Some(n.to_string());
                }
            }
            Some("message") => {
                if first_user.is_none()
                    && value.pointer("/message/role").and_then(|r| r.as_str()) == Some("user")
                {
                    if let Some(content) = value.pointer("/message/content") {
                        let text = content_text(content);
                        if !text.is_empty() {
                            first_user = Some(text);
                        }
                    }
                }
            }
            _ => {}
        }
        if name.is_some() && first_user.is_some() {
            break;
        }
    }

    match name.or(first_user) {
        Some(n) => shorten_for_display(&n, 60),
        None => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Session".to_string()),
    }
}

/// Fallback model source: read custom providers from `~/.pi/agent/models.json`
/// (used when the `get_available_models` RPC returns nothing).
fn models_from_config_file() -> Vec<ModelInfo> {
    let Ok(home) = std::env::var("HOME") else { return Vec::new() };
    let path = std::path::Path::new(&home).join(".pi/agent/models.json");
    let Ok(content) = std::fs::read_to_string(&path) else { return Vec::new() };
    let Ok(value) = serde_json::from_str::<Value>(&content) else { return Vec::new() };
    let Some(providers) = value.get("providers").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut models = Vec::new();
    for (provider, prov_val) in providers {
        let Some(arr) = prov_val.get("models").and_then(|m| m.as_array()) else { continue };
        for m in arr {
            let Some(id) = m.get("id").and_then(|i| i.as_str()) else { continue };
            models.push(ModelInfo::new(provider, id));
        }
    }
    models
}

// ================= welcome page =================

/// Button content with both an icon and a label (GTK4 buttons show only one
/// of the two when set via the `label`/`icon-name` properties).
fn icon_label_box(icon_name: &str, text: &str) -> Box {
    let row = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(Align::Center)
        .build();
    let icon = Image::builder().icon_name(icon_name).build();
    let label = Label::builder().label(text).build();
    row.append(&icon);
    row.append(&label);
    row
}

fn build_welcome_page() -> (Widget, Button) {
    let page = Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .halign(Align::Center)
        .valign(Align::Center)
        .spacing(20)
        .margin_top(40)
        .build();

    let avatar_circle = Box::builder()
        .orientation(Orientation::Vertical)
        .halign(Align::Center)
        .width_request(84)
        .height_request(84)
        .css_classes(["welcome-avatar"])
        .build();
    let avatar = Label::builder()
        .label("π")
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    avatar.add_css_class("welcome-avatar-label");
    avatar_circle.append(&avatar);

    let title = Label::builder().label("pi").halign(Align::Center).build();
    title.add_css_class("welcome-title");

    let subtitle = Label::builder()
        .label("Start a new chat to write, explain, or fix code with the pi coding agent.")
        .halign(Align::Center)
        .wrap(true)
        .max_width_chars(42)
        .justify(gtk4::Justification::Center)
        .build();
    subtitle.add_css_class("welcome-subtitle");

    let new_chat = Button::builder()
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Center)
        .valign(Align::Center)
        .hexpand(false)
        .build();
    new_chat.set_child(Some(&icon_label_box("document-new-symbolic", "New chat")));

    page.append(&avatar_circle);
    page.append(&title);
    page.append(&subtitle);
    page.append(&new_chat);

    (page.upcast::<Widget>(), new_chat)
}
