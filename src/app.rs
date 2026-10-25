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
    Orientation, Overlay, PropertyExpression, ScrolledWindow, Separator, StringList, Stack,
    TextView, Widget,
};
use libadwaita::prelude::*;
use libadwaita::{
    ComboRow, HeaderBar, MessageDialog, PreferencesGroup, PreferencesPage, PreferencesWindow,
    SwitchRow, Toast, WindowTitle,
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
    NewSession(Receiver<Value>),
    Prompt(Receiver<Value>),
    SwitchSession(Receiver<Value>),
    GetMessages(Receiver<Value>),
}

struct State {
    rpc: PiRpcClient,
    events: PiEventStream,

    window: gtk4::ApplicationWindow,
    stack: Stack,
    chat_view: ChatView,
    session_list: SessionList,

    window_title: WindowTitle,
    model_dropdown: DropDown,

    all_models: Vec<ModelInfo>,
    filtered_models: Vec<ModelInfo>,
    current_model_label: Option<String>,

    input_view: TextView,
    input_buffer: gtk4::TextBuffer,
    input_placeholder: Label,
    send_button: Button,
    stop_button: Button,

    streaming: Option<StreamingBubble>,
    tool_blocks: HashMap<String, ToolBlock>,

    pending: Pending,
    pending_models: Option<Receiver<Value>>,
    pending_state: Option<(Receiver<Value>, bool)>,
    pending_levels: Option<Receiver<Value>>,

    /// The agent is running a prompt (input still accepts steering messages).
    busy: bool,
    /// The current session already has a display name (don't auto-rename).
    session_named: bool,

    current_session_file: Option<String>,
    sessions: Vec<(String, String, String)>, // (file path, display name, time label)

    suppress_model_signal: Rc<Cell<bool>>,
    suppress_thinking_signal: Rc<Cell<bool>>,
    suppress_autocompact: Rc<Cell<bool>>,

    thinking_levels: Vec<String>,
    current_thinking_level: Option<String>,

    thinking_combo_row: ComboRow,
    autoswitch_row: SwitchRow,
}

pub struct PiWindow {
    inner: Rc<RefCell<State>>,
}

impl PiWindow {
    pub fn new(app: &libadwaita::Application) -> Self {
        let (rpc, events) = crate::rpc::spawn().expect("failed to start pi RPC");

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

        window_title.set_subtitle("GNOME AI coding assistant");
        header.set_title_widget(Some(&window_title));

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
            .icon_name("preferences-system-symbolic")
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

        // Chat page
        let chat_page = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .css_classes(["chat-page"])
            .build();

        let chat_view = ChatView::new();
        chat_page.append(&chat_view.scrolled);

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
        chat_page.append(&input_card);

        stack.add_titled(&chat_page, Some("chat"), "Chat");

        // Welcome page (initial view).
        let (welcome_page, welcome_new_chat) = build_welcome_page();
        stack.add_titled(&welcome_page, Some("welcome"), "Welcome");
        stack.set_visible_child_name("welcome");

        main_box.append(&sidebar_scrolled);
        main_box.append(&separator);
        main_box.append(&stack);

        let overlay = chat_view.overlay.clone();
        overlay.set_child(Some(&main_box));

        window.set_child(Some(&overlay));

        // ================= State =================
        let state = Rc::new(RefCell::new(State {
            rpc,
            events,
            window,
            stack,
            chat_view,
            session_list,
            window_title,
            model_dropdown,
            all_models: Vec::new(),
            filtered_models: Vec::new(),
            current_model_label: None,
            input_view,
            input_buffer,
            input_placeholder,
            send_button,
            stop_button,
            streaming: None,
            tool_blocks: HashMap::new(),
            pending: Pending::None,
            pending_models: None,
            pending_state: None,
            pending_levels: None,
            busy: false,
            session_named: false,
            current_session_file: None,
            sessions: Vec::new(),
            suppress_model_signal: Rc::new(Cell::new(false)),
            suppress_thinking_signal: Rc::new(Cell::new(false)),
            suppress_autocompact: Rc::new(Cell::new(false)),
            thinking_levels: Vec::new(),
            current_thinking_level: None,
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
            s.borrow_mut().on_switch_session(id);
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
            if let Ok(st) = s.try_borrow() {
                if let Some(level) = st.thinking_levels.get(idx).cloned() {
                    drop(st);
                    if let Ok(mut st) = s.try_borrow_mut() {
                        st.set_thinking_level(level);
                    }
                }
            }
        });

        let s = state.clone();
        let suppress = s.borrow().suppress_autocompact.clone();
        let switch = s.borrow().autoswitch_row.clone();
        switch.connect_active_notify(move |row| {
            if suppress.get() {
                return;
            }
            let enabled = row.is_active();
            if let Ok(st) = s.try_borrow_mut() {
                let _ = st.rpc.send(PiCommand::SetAutoCompaction { enabled });
            }
        });

        // ================= Kick off =================

        {
            let mut st = state.borrow_mut();
            st.load_disk_sessions();
            st.pending_state = Some((st.rpc.send(PiCommand::GetState), false));
            st.pending_models = Some(st.rpc.send(PiCommand::GetAvailableModels));
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
    fn on_new_chat(&mut self) {
        self.stack.set_visible_child_name("chat");
        self.chat_view.clear();
        self.streaming = None;
        self.tool_blocks.clear();
        self.current_session_file = None;
        self.session_named = false;
        self.busy = false;
        self.window_title.set_title("New chat");
        self.window_title.set_subtitle("Start a conversation");
        self.stop_button.set_sensitive(false);
        self.input_view.grab_focus();

        let receiver = self.rpc.send(PiCommand::NewSession { parent_session: None });
        self.pending = Pending::NewSession(receiver);

        self.show_toast("New chat started");
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
        self.input_buffer.set_text("");

        self.stack.set_visible_child_name("chat");

        // User bubble.
        let time = chat::now_time();
        self.chat_view.append(&chat::user_message(&trimmed, Some(&time)).box_);

        // Name the session after the first user message.
        if !self.session_named {
            if let Some(file) = self.current_session_file.clone() {
                let name = shorten_for_display(&trimmed, 48);
                if !name.is_empty() {
                    let _ = self.rpc.send(PiCommand::SetSessionName { name: name.clone() });
                    self.session_list.rename_session(&file, &name);
                    self.window_title.set_title(&name);
                    if let Some(entry) =
                        self.sessions.iter_mut().find(|(f, _, _)| *f == file)
                    {
                        entry.1 = name;
                    }
                }
                self.session_named = true;
            }
        }

        if self.busy {
            // The agent is already running: queue a steering message that is
            // delivered after the current assistant turn finishes its tool
            // calls.
            let _ = self.rpc.send(PiCommand::Steer { message: trimmed });
            self.show_toast("Queued — sent when the current turn finishes");
        } else {
            // Assistant streaming bubble.
            let streaming = StreamingBubble::new();
            self.chat_view.append(&streaming.container);
            self.streaming = Some(streaming);
            self.chat_view.scroll_to_bottom();

            self.busy = true;
            self.stop_button.set_sensitive(true);
            self.window_title.set_subtitle("Working…");

            let receiver = self.rpc.send(PiCommand::Prompt { message: trimmed });
            self.pending = Pending::Prompt(receiver);
        }
    }

    fn on_stop(&mut self) {
        let _ = self.rpc.send(PiCommand::Abort);
        self.stop_button.set_sensitive(false);
    }

    fn on_switch_session(&mut self, id: &str) {
        if self.current_session_file.as_deref() == Some(id) {
            self.stack.set_visible_child_name("chat");
            return;
        }
        let Some(name) = self
            .sessions
            .iter()
            .find(|(f, _, _)| *f == id)
            .map(|(_, n, _)| n.clone())
        else {
            return;
        };

        self.current_session_file = Some(id.to_string());
        self.session_named = true;
        self.window_title.set_title(&name);
        self.window_title.set_subtitle("Resumed chat");
        self.stack.set_visible_child_name("chat");
        self.chat_view.clear();
        self.streaming = None;
        self.tool_blocks.clear();
        self.session_list.set_active(id);
        self.input_view.grab_focus();

        let receiver = self.rpc.send(PiCommand::SwitchSession { session_path: id.to_string() });
        self.pending = Pending::SwitchSession(receiver);
    }

    // ================= event/response polling =================

    fn poll_events(&mut self) {
        // Drain async events first.
        while let Some(event) = self.events.try_event() {
            self.handle_event(&event);
        }

        if let Some(recv) = self.pending_models.take() {
            match recv.try_recv() {
                Ok(value) => self.load_models_done(&value),
                Err(TryRecvError::Empty) => self.pending_models = Some(recv),
                Err(TryRecvError::Disconnected) => self.show_toast("Model list unavailable"),
            }
        }

        if let Some((recv, after_new)) = self.pending_state.take() {
            match recv.try_recv() {
                Ok(value) => self.load_state_done(&value, after_new),
                Err(TryRecvError::Empty) => self.pending_state = Some((recv, after_new)),
                Err(TryRecvError::Disconnected) => self.show_toast("State sync failed"),
            }
        }

        if let Some(recv) = self.pending_levels.take() {
            match recv.try_recv() {
                Ok(value) => self.load_levels_done(&value),
                Err(TryRecvError::Empty) => self.pending_levels = Some(recv),
                Err(TryRecvError::Disconnected) => {}
            }
        }

        match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::None => {}
            Pending::NewSession(recv) => match recv.try_recv() {
                Ok(value) => {
                    let ok = value.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                    let cancelled = value
                        .pointer("/data/cancelled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !ok {
                        self.show_toast("Failed to start a new chat");
                    } else if cancelled {
                        self.show_toast("New chat cancelled by an extension");
                    } else {
                        self.pending_state =
                            Some((self.rpc.send(PiCommand::GetState), true));
                    }
                }
                Err(TryRecvError::Empty) => self.pending = Pending::NewSession(recv),
                Err(TryRecvError::Disconnected) => {
                    self.show_toast("Failed to start a new chat — RPC bridge died")
                }
            },
            Pending::Prompt(recv) => match recv.try_recv() {
                Ok(value) => {
                    let ok = value.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
                    if !ok {
                        let err = value
                            .get("error")
                            .and_then(|e| e.as_str())
                            .unwrap_or("unknown error");
                        self.show_toast(&format!("Prompt failed: {err}"));
                        self.busy = false;
                        self.stop_button.set_sensitive(false);
                        self.window_title.set_subtitle("Idle");
                        self.finalize_streaming(None, None);
                    }
                }
                Err(TryRecvError::Empty) => self.pending = Pending::Prompt(recv),
                Err(TryRecvError::Disconnected) => {
                    self.show_toast("Prompt failed — RPC bridge died");
                    self.busy = false;
                    self.stop_button.set_sensitive(false);
                    self.window_title.set_subtitle("Idle");
                    self.finalize_streaming(None, None);
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
                        self.pending = Pending::GetMessages(self.rpc.send(PiCommand::GetMessages));
                    } else {
                        self.show_toast("Session switch cancelled");
                    }
                }
                Err(TryRecvError::Empty) => self.pending = Pending::SwitchSession(recv),
                Err(TryRecvError::Disconnected) => {
                    self.show_toast("Session switch failed — RPC bridge died")
                }
            },
            Pending::GetMessages(recv) => match recv.try_recv() {
                Ok(value) => self.load_history_done(&value),
                Err(TryRecvError::Empty) => self.pending = Pending::GetMessages(recv),
                Err(TryRecvError::Disconnected) => {}
            },
        }
    }

    fn load_history_done(&mut self, value: &Value) {
        if let Some(messages) = value.pointer("/data/messages") {
            self.render_messages(messages);
        }
        self.chat_view.scroll_to_bottom();
    }

    fn render_messages(&self, messages: &Value) {
        let Some(arr) = messages.as_array() else { return };
        for msg in arr {
            let content = msg.get("content").cloned().unwrap_or_default();
            let text = content_text(&content);
            let thinking = content_thinking(&content);
            let time = message_time(msg);
            let time = time.as_deref();
            match message_role(msg) {
                Some("user") if !text.is_empty() => {
                    self.chat_view.append(&chat::user_message(&text, time).box_)
                }
                Some("assistant") if !text.is_empty() || !thinking.is_empty() => {
                    let thinking = if thinking.is_empty() { None } else { Some(thinking.as_str()) };
                    self.chat_view.append(
                        &chat::assistant_message(&text, thinking, time).box_,
                    )
                }
                _ => {}
            }
        }
    }

    // ================= model list =================

    fn load_models_done(&mut self, value: &Value) {
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
            self.show_toast("No models available");
            return;
        }
        models.sort_by(|a, b| a.label.cmp(&b.label));
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

    fn select_current_model(&mut self) {
        let Some(label) = self.current_model_label.clone() else { return };
        if let Some(idx) = self.filtered_models.iter().position(|m| m.label == label) {
            self.suppress_model_signal.set(true);
            self.model_dropdown.set_selected(idx as u32);
            self.suppress_model_signal.set(false);
        }
    }

    fn set_model(&mut self, info: ModelInfo) {
        let _ = self
            .rpc
            .send(PiCommand::SetModel { provider: info.provider, model_id: info.id });
        self.current_model_label = Some(info.label.clone());
        self.show_toast(&format!("Model: {}", info.label));
    }

    // ================= state sync =================

    fn load_state_done(&mut self, value: &Value, after_new: bool) {
        let empty = serde_json::Value::Null;
        let data = value.get("data").unwrap_or(&empty);

        // Model.
        if let Some(model) = data.get("model") {
            if let (Some(provider), Some(id)) = (
                model.get("provider").and_then(|p| p.as_str()),
                model.get("id").and_then(|i| i.as_str()),
            ) {
                self.current_model_label = Some(ModelInfo::new(provider, id).label);
                self.select_current_model();
            }
        }

        // Thinking level.
        if let Some(level) = data.get("thinkingLevel").and_then(|l| l.as_str()) {
            self.select_thinking_level(level);
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
            self.current_session_file = Some(file.clone());
            let time_label = time_label_now();
            if !self.sessions.iter().any(|(f, _, _)| *f == file) {
                self.sessions.push((file.clone(), name.clone(), time_label.clone()));
            } else if let Some(entry) = self.sessions.iter_mut().find(|(f, _, _)| *f == file) {
                entry.1 = name.clone();
            }
            self.session_list.add_session(&file, &name, &time_label, true);
            self.session_list.set_active(&file);
            self.window_title.set_title(&name);
        }

        // Auto-compaction.
        if let Some(enabled) = data.get("autoCompactionEnabled").and_then(|v| v.as_bool()) {
            self.suppress_autocompact.set(true);
            self.autoswitch_row.set_active(enabled);
            self.suppress_autocompact.set(false);
        }

        // Thinking levels for the current model.
        self.pending_levels = Some(self.rpc.send(PiCommand::GetAvailableThinkingLevels));
    }

    fn load_levels_done(&mut self, value: &Value) {
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

        let string_list = StringList::new(&[]);
        for level in &levels {
            string_list.append(level);
        }
        self.suppress_thinking_signal.set(true);
        self.thinking_combo_row.set_model(Some(&string_list));
        self.thinking_levels = levels;
        if let Some(current) = self.current_thinking_level.clone() {
            if let Some(idx) = self.thinking_levels.iter().position(|l| *l == current) {
                self.thinking_combo_row.set_selected(idx as u32);
            }
        }
        self.suppress_thinking_signal.set(false);
    }

    fn select_thinking_level(&mut self, level: &str) {
        self.current_thinking_level = Some(level.to_string());
        if let Some(idx) = self.thinking_levels.iter().position(|l| l == level) {
            self.suppress_thinking_signal.set(true);
            self.thinking_combo_row.set_selected(idx as u32);
            self.suppress_thinking_signal.set(false);
        }
    }

    fn set_thinking_level(&mut self, level: String) {
        self.current_thinking_level = Some(level.clone());
        let _ = self.rpc.send(PiCommand::SetThinkingLevel { level });
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

    fn handle_event(&mut self, event: &PiEvent) {
        match event {
            PiEvent::AgentStart => {
                self.busy = true;
                self.window_title.set_subtitle("Working…");
                self.stop_button.set_sensitive(true);
            }
            PiEvent::AgentSettled { aborted } => {
                self.busy = false;
                self.window_title.set_subtitle("Idle");
                self.stop_button.set_sensitive(false);
                self.finalize_streaming(None, None);
                self.chat_view.scroll_to_bottom();
                if *aborted == Some(true) {
                    self.show_toast("Stopped");
                }
            }
            PiEvent::AgentEnd | PiEvent::TurnStart | PiEvent::TurnEnd | PiEvent::Other => {}
            PiEvent::Disconnected => {
                self.busy = false;
                self.stop_button.set_sensitive(false);
                self.send_button.set_sensitive(false);
                self.input_view.set_sensitive(false);
                self.window_title.set_subtitle("pi disconnected — restart the app");
                self.show_toast("pi exited — RPC disconnected, please restart pi-gui");
            }
            PiEvent::Response { .. } => {}
            PiEvent::MessageStart { message } => {
                if message.as_ref().and_then(message_role) == Some("assistant") {
                    self.ensure_streaming();
                }
            }
            PiEvent::MessageUpdate { event } => self.handle_message_update(event.as_ref()),
            PiEvent::MessageEnd { message } => {
                if let Some(message) = message {
                    if message_role(message) == Some("assistant") {
                        let content = message.get("content").cloned().unwrap_or_default();
                        let text = content_text(&content);
                        let thinking = content_thinking(&content);
                        let thinking = if thinking.is_empty() { None } else { Some(thinking) };
                        self.finalize_streaming(Some(&text), thinking.as_deref());
                    }
                }
            }
            PiEvent::ToolExecutionStart { tool_call_id, tool_name, args } => {
                let name = tool_name.clone().unwrap_or_else(|| "tool".into());
                let summary = tool_summary(args.as_ref());
                let block = ToolBlock::new(&name, &summary);
                self.chat_view.append(&block.box_);
                if let Some(id) = tool_call_id {
                    self.tool_blocks.insert(id.clone(), block);
                }
                self.chat_view.scroll_to_bottom();
            }
            PiEvent::ToolExecutionUpdate { tool_call_id, partial_result } => {
                let text = partial_result.as_ref().map(result_text).unwrap_or_default();
                if let (Some(id), false) = (tool_call_id, text.is_empty()) {
                    if let Some(block) = self.tool_blocks.get(id) {
                        block.set_content(&text);
                    }
                }
            }
            PiEvent::ToolExecutionEnd { tool_call_id, result, is_error } => {
                let text = result.as_ref().map(result_text).unwrap_or_default();
                if let Some(id) = tool_call_id {
                    if let Some(block) = self.tool_blocks.get(id) {
                        if !text.is_empty() {
                            block.set_content(&text);
                        }
                        block.set_finished(*is_error == Some(true));
                    }
                }
            }
            PiEvent::EntryAppended { .. } => {}
            PiEvent::AutoRetryStart { attempt, error_message } => {
                if let Some(attempt) = attempt {
                    let reason = error_message
                        .as_deref()
                        .map(|s| shorten_for_display(s, 80))
                        .unwrap_or_default();
                    let msg = if reason.is_empty() {
                        format!("Retrying (attempt {attempt})…")
                    } else {
                        format!("Retrying (attempt {attempt}): {reason}")
                    };
                    self.show_toast(&msg);
                }
            }
            PiEvent::AutoRetryEnd { success: Some(false), final_error, .. } => {
                let err = final_error.clone().unwrap_or_default();
                self.show_toast(&format!("Retry failed: {err}"));
            }
            PiEvent::AutoRetryEnd { .. } => {}
            PiEvent::SessionInfoChanged { name } => {
                if let (Some(name), Some(file)) = (name, self.current_session_file.clone()) {
                    self.session_list.rename_session(&file, name);
                    self.window_title.set_title(name);
                    if let Some(entry) = self.sessions.iter_mut().find(|(f, _, _)| *f == file) {
                        entry.1 = name.clone();
                    }
                }
            }
            PiEvent::ThinkingLevelChanged { level } => {
                if let Some(level) = level {
                    self.select_thinking_level(level);
                }
            }
            PiEvent::ExtensionUiRequest { id, method, extra } => {
                self.handle_extension_request(id, method, extra);
            }
            PiEvent::ExtensionError { extension_path, error, .. } => {
                eprintln!("[extension error] {extension_path}: {error}");
                self.show_toast(&format!("Extension error: {error}"));
            }
        }
    }

    fn handle_message_update(&mut self, event: Option<&Value>) {
        let Some(event) = event else { return };
        match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text_delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.ensure_streaming();
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.append_text(delta);
                    }
                    self.chat_view.scroll_to_bottom();
                }
            }
            "thinking_delta" => {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    self.ensure_streaming();
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.append_thinking(delta);
                    }
                    self.chat_view.scroll_to_bottom();
                }
            }
            "text_end" => {
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.set_text(content);
                    }
                }
            }
            "thinking_end" => {
                if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                    if let Some(streaming) = self.streaming.as_ref() {
                        streaming.set_thinking(content);
                    }
                }
            }
            "error" => {
                let msg = event.get("error").and_then(|e| e.as_str()).unwrap_or("stream error");
                self.show_toast(&format!("Stream error: {msg}"));
            }
            _ => {}
        }
    }

    fn ensure_streaming(&mut self) {
        if self.streaming.is_none() {
            let streaming = StreamingBubble::new();
            self.chat_view.append(&streaming.container);
            self.streaming = Some(streaming);
            self.chat_view.scroll_to_bottom();
        }
    }

    /// Replace the streaming bubble with a final assistant bubble. With
    /// `text: None` any dangling streaming bubble is just removed.
    fn finalize_streaming(&mut self, text: Option<&str>, thinking: Option<&str>) {
        if let Some(streaming) = self.streaming.take() {
            let text = text.unwrap_or("");
            let thinking = thinking.unwrap_or("");
            let has_content = !text.is_empty() || !thinking.is_empty();
            if has_content {
                let bubble = chat::assistant_message(
                    text,
                    if thinking.is_empty() { None } else { Some(thinking) },
                    None,
                );
                // Insert at the streaming bubble's position so tool blocks
                // that already followed it stay in order.
                self.chat_view.list.insert_child_after(&bubble.box_, Some(&streaming.container));
            }
            self.chat_view.list.remove(&streaming.container);
        }
    }

    // ================= extension UI requests =================

    fn handle_extension_request(&self, id: &str, method: &str, extra: &Value) {
        match method {
            "confirm" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Confirm");
                let message = extra.get("message").and_then(|m| m.as_str()).unwrap_or("");
                self.show_confirm(id, title, message);
            }
            "input" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Input");
                let placeholder = extra.get("placeholder").and_then(|p| p.as_str()).unwrap_or("");
                self.show_input(id, title, placeholder);
            }
            "editor" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Edit text");
                let prefill = extra.get("prefill").and_then(|p| p.as_str()).unwrap_or("");
                self.show_editor(id, title, prefill);
            }
            "select" => {
                let title = extra.get("title").and_then(|t| t.as_str()).unwrap_or("Select");
                self.show_select(id, title, extra);
            }
            // Fire-and-forget methods: display, never respond.
            "notify" => {
                let message = extra.get("message").and_then(|m| m.as_str()).unwrap_or("");
                self.show_toast(message);
            }
            "setStatus" | "setWidget" | "setTitle" | "set_editor_text" | _ => {}
        }
    }

    fn show_confirm(&self, id: &str, title: &str, message: &str) {
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
        let rpc = self.rpc.clone();
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

    fn show_input(&self, id: &str, title: &str, placeholder: &str) {
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
        let rpc = self.rpc.clone();
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

    fn show_editor(&self, id: &str, title: &str, prefill: &str) {
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
        let rpc = self.rpc.clone();
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

    fn show_select(&self, id: &str, title: &str, extra: &Value) {
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
        let rpc = self.rpc.clone();
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

    fn show_toast(&self, message: &str) {
        if message.is_empty() {
            return;
        }
        // Toast titles are parsed as Pango markup; escape anything the model
        // or an extension emitted (e.g. "<url>" in a warning message).
        let title = glib::markup_escape_text(message);
        let toast = Toast::builder().title(title).build();
        self.chat_view.overlay.add_toast(toast);
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
        .width_request(84)
        .height_request(84)
        .css_classes(["welcome-avatar"])
        .build();
    let avatar = Label::builder()
        .label("π")
        .valign(Align::Center)
        .hexpand(true)
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
