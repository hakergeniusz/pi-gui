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
    Align, Box, Button, DropDown, Entry, Frame, HeaderBar, Image, Label, ListBox, ListBoxRow,
    Orientation, ScrolledWindow, Separator, StringList, Stack, TextView, Widget,
};
use libadwaita::prelude::*;
use libadwaita::{
    ComboRow, MessageDialog, PreferencesGroup, PreferencesPage, PreferencesWindow, SwitchRow,
    Toast,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

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

    title_label: Label,
    subtitle_label: Label,
    model_dropdown: DropDown,

    all_models: Vec<ModelInfo>,
    filtered_models: Vec<ModelInfo>,
    current_model_label: Option<String>,

    input_entry: Entry,
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
    sessions: Vec<(String, String)>, // (file path, display name)

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
    pub fn new(app: &gtk4::Application) -> Self {
        let (rpc, events) = crate::rpc::spawn().expect("failed to start pi RPC");

        let window = gtk4::ApplicationWindow::builder()
            .application(app)
            .default_width(1100)
            .default_height(760)
            .build();

        // ================= Header bar =================
        let header = HeaderBar::new();

        let title_box = Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(2)
            .margin_start(6)
            .build();

        let title_label = Label::builder()
            .label("pi coding agent")
            .halign(Align::Start)
            .hexpand(false)
            .build();
        title_label.add_css_class("title");

        let subtitle_label = Label::builder()
            .label("GNOME AI coding assistant")
            .halign(Align::Start)
            .hexpand(false)
            .build();
        subtitle_label.add_css_class("subtitle");

        title_box.append(&title_label);
        title_box.append(&subtitle_label);
        header.set_title_widget(Some(&title_box));

        // New chat button (start of header)
        let new_chat_btn = Button::builder()
            .icon_name("document-new-symbolic")
            .css_classes(["flat", "pill"])
            .valign(Align::Center)
            .build();
        new_chat_btn.set_tooltip_text(Some("New chat"));
        header.pack_start(&new_chat_btn);

        // Model search (end)
        let model_search_entry = Entry::builder()
            .placeholder_text("Search models…")
            .hexpand(false)
            .valign(Align::Center)
            .width_request(170)
            .build();
        model_search_entry.add_css_class("model-search");
        model_search_entry.set_tooltip_text(Some("Filter models"));
        header.pack_end(&model_search_entry);

        // Model dropdown (end)
        let model_list = StringList::new(&["Loading models…"]);
        let model_dropdown = DropDown::builder()
            .model(&model_list)
            .hexpand(false)
            .valign(Align::Center)
            .build();
        model_dropdown.set_tooltip_text(Some("Model"));
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
            .width_request(260)
            .build();
        sidebar_scrolled.add_css_class("sidebar");

        let session_list = SessionList::new();

        let new_chat_row = Button::builder()
            .label("New chat")
            .icon_name("document-new-symbolic")
            .hexpand(true)
            .halign(Align::Start)
            .css_classes(["flat", "session-row", "new-chat-row"])
            .margin_top(10)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();

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

        // Input area
        let input_frame = Frame::builder()
            .hexpand(true)
            .css_classes(["input-frame"])
            .build();

        let input_box = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(14)
            .margin_end(14)
            .build();

        let input_entry = Entry::builder()
            .placeholder_text("Ask pi to write, explain, or fix code…")
            .hexpand(true)
            .valign(Align::Center)
            .build();
        input_entry.add_css_class("chat-input");

        let send_button = Button::builder()
            .label("Send")
            .icon_name("send-symbolic")
            .css_classes(["suggested-action"])
            .valign(Align::Center)
            .build();
        send_button.set_sensitive(false);

        let stop_button = Button::builder()
            .icon_name("process-stop-symbolic")
            .css_classes(["destructive-action"])
            .valign(Align::Center)
            .build();
        stop_button.set_sensitive(false);
        stop_button.set_tooltip_text(Some("Stop"));

        input_box.append(&input_entry);
        input_box.append(&stop_button);
        input_box.append(&send_button);
        input_frame.set_child(Some(&input_box));
        chat_page.append(&input_frame);

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
            title_label,
            subtitle_label,
            model_dropdown,
            all_models: Vec::new(),
            filtered_models: Vec::new(),
            current_model_label: None,
            input_entry,
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
        let entry = s.borrow().input_entry.clone();
        entry.connect_activate(move |_| s.borrow_mut().on_send_message());

        // Send button sensitivity follows input text.
        let s = state.clone();
        let send = s.borrow().send_button.clone();
        s.borrow()
            .input_entry
            .connect_changed(move |entry| send.set_sensitive(!entry.text().is_empty()));

        let s = state.clone();
        model_search_entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            let query = if text.is_empty() { None } else { Some(text) };
            s.borrow_mut().filter_models(query.as_deref());
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
        self.title_label.set_label("New chat");
        self.subtitle_label.set_label("Start a conversation");
        self.stop_button.set_sensitive(false);
        self.input_entry.grab_focus();

        let receiver = self.rpc.send(PiCommand::NewSession { parent_session: None });
        self.pending = Pending::NewSession(receiver);

        self.show_toast("New chat started");
    }

    fn on_send_message(&mut self) {
        let text = self.input_entry.text().to_string();
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.input_entry.set_text("");

        self.stack.set_visible_child_name("chat");

        // User bubble.
        self.chat_view.append(&chat::user_message(&trimmed).box_);

        // Name the session after the first user message.
        if !self.session_named {
            if let Some(file) = self.current_session_file.clone() {
                let name = shorten_for_display(&trimmed, 48);
                if !name.is_empty() {
                    let _ = self.rpc.send(PiCommand::SetSessionName { name: name.clone() });
                    self.session_list.rename_session(&file, &name);
                    self.title_label.set_label(&name);
                    if let Some(entry) = self.sessions.iter_mut().find(|(f, _)| *f == file) {
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
            self.subtitle_label.set_label("Working…");

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
        let Some(name) = self.sessions.iter().find(|(f, _)| f == id).map(|(_, n)| n.clone())
        else {
            return;
        };

        self.current_session_file = Some(id.to_string());
        self.session_named = true;
        self.title_label.set_label(&name);
        self.subtitle_label.set_label("Resumed chat");
        self.stack.set_visible_child_name("chat");
        self.chat_view.clear();
        self.streaming = None;
        self.tool_blocks.clear();
        self.session_list.set_active(id);
        self.input_entry.grab_focus();

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
                Err(_) => self.pending_models = Some(recv),
            }
        }

        if let Some((recv, after_new)) = self.pending_state.take() {
            match recv.try_recv() {
                Ok(value) => self.load_state_done(&value, after_new),
                Err(_) => self.pending_state = Some((recv, after_new)),
            }
        }

        if let Some(recv) = self.pending_levels.take() {
            match recv.try_recv() {
                Ok(value) => self.load_levels_done(&value),
                Err(_) => self.pending_levels = Some(recv),
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
                Err(_) => self.pending = Pending::NewSession(recv),
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
                        self.subtitle_label.set_label("Idle");
                        self.finalize_streaming(None, None);
                    }
                }
                Err(_) => self.pending = Pending::Prompt(recv),
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
                Err(_) => self.pending = Pending::SwitchSession(recv),
            },
            Pending::GetMessages(recv) => match recv.try_recv() {
                Ok(value) => self.load_history_done(&value),
                Err(_) => self.pending = Pending::GetMessages(recv),
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
            let text = msg.get("content").map(content_text).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            match message_role(msg) {
                Some("user") => self.chat_view.append(&chat::user_message(&text).box_),
                Some("assistant") => {
                    self.chat_view.append(&chat::assistant_message(&text, None).box_)
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
        self.filter_models(None);
    }

    /// Rebuild the dropdown from `all_models` filtered by `query` (case
    /// insensitive substring), then re-select the active model.
    fn filter_models(&mut self, query: Option<&str>) {
        let query = query.unwrap_or("").to_lowercase();
        self.filtered_models = self
            .all_models
            .iter()
            .filter(|m| query.is_empty() || m.label.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();

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
            if let Some(name) = model.get("name").and_then(|n| n.as_str()) {
                self.subtitle_label.set_label(name);
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
            if !self.sessions.iter().any(|(f, _)| *f == file) {
                self.sessions.push((file.clone(), name.clone()));
            } else if let Some(entry) = self.sessions.iter_mut().find(|(f, _)| *f == file) {
                entry.1 = name.clone();
            }
            self.session_list.add_session(&file, &name, true);
            self.session_list.set_active(&file);
            if self.title_label.text().is_empty() || after_new {
                self.title_label.set_label(&name);
            }
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
        for (path, name) in &sessions {
            self.session_list.add_session(path, name, false);
        }
        self.sessions = sessions;
    }

    // ================= agent events =================

    fn handle_event(&mut self, event: &PiEvent) {
        match event {
            PiEvent::AgentStart => {
                self.busy = true;
                self.subtitle_label.set_label("Working…");
                self.stop_button.set_sensitive(true);
            }
            PiEvent::AgentSettled { aborted } => {
                self.busy = false;
                self.subtitle_label.set_label("Idle");
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
                self.input_entry.set_sensitive(false);
                self.subtitle_label.set_label("pi disconnected — restart the app");
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
            PiEvent::ToolExecutionStart { tool_call_id, tool_name } => {
                let name = tool_name.clone().unwrap_or_else(|| "tool".into());
                let block = ToolBlock::new(&name);
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
                        block.set_error(*is_error == Some(true));
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
                    self.title_label.set_label(name);
                    if let Some(entry) = self.sessions.iter_mut().find(|(f, _)| *f == file) {
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
                let bubble = chat::assistant_message(text, if thinking.is_empty() { None } else { Some(thinking) });
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
        let toast = Toast::builder().title(message).build();
        self.chat_view.overlay.add_toast(toast);
    }
}

// ================= session discovery helpers =================

/// Scan pi's session store (`~/.pi/agent/sessions/`) for recent `.jsonl`
/// session files, newest first (max 40).
fn scan_disk_sessions() -> Vec<(String, String)> {
    let Some(home) = std::env::var_os("HOME") else { return Vec::new() };
    let root = std::path::Path::new(&home).join(".pi/agent/sessions");
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
    collect_session_files(&root, &mut files, 0);
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
        .into_iter()
        .take(40)
        .map(|(path, _)| {
            let name = session_display_name(&path);
            (path.to_string_lossy().into_owned(), name)
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

fn build_welcome_page() -> (Widget, Button) {
    let page = Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .vexpand(true)
        .halign(Align::Center)
        .valign(Align::Center)
        .spacing(24)
        .build();

    let icon = Image::builder()
        .icon_name("accessories-text-editor-symbolic")
        .pixel_size(80)
        .build();
    icon.add_css_class("welcome-icon");

    let title = Label::builder()
        .label("Welcome to pi")
        .halign(Align::Center)
        .build();
    title.add_css_class("welcome-title");

    let subtitle = Label::builder()
        .label("Start a new chat to write, explain, or fix code with the pi coding agent.")
        .halign(Align::Center)
        .wrap(true)
        .max_width_chars(40)
        .justify(gtk4::Justification::Center)
        .build();
    subtitle.add_css_class("welcome-subtitle");

    let new_chat = Button::builder()
        .label("New chat")
        .icon_name("document-new-symbolic")
        .css_classes(["suggested-action", "pill"])
        .halign(Align::Center)
        .valign(Align::Center)
        .hexpand(false)
        .build();

    page.append(&icon);
    page.append(&title);
    page.append(&subtitle);
    page.append(&new_chat);

    (page.upcast::<Widget>(), new_chat)
}
