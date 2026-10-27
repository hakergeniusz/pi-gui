//! Chat view widgets following GNOME HIG / libadwaita styling.
//!
//! Transcript conventions match modern chat apps (ChatGPT / Claude desktop):
//! user messages are right-aligned tinted bubbles; assistant messages are
//! plain full-width text under an avatar + name header. Reasoning ("thinking")
//! is a collapsible dimmed block, and tool runs render as cards with a
//! monospace result body.

use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, Image, Label, Orientation, Revealer, ScrolledWindow, Spinner, ToggleButton,
};
use libadwaita::ToastOverlay;

/// A single rendered chat message row.
pub struct MessageBubble {
    pub box_: Box,
}

pub fn now_time() -> String {
    glib::DateTime::now_local()
        .and_then(|d| d.format("%H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Small circular "π" avatar for the assistant.
fn avatar(size: i32) -> Box {
    let circle = Box::builder()
        .orientation(Orientation::Vertical)
        .halign(Align::Start)
        .valign(Align::Start)
        .width_request(size)
        .height_request(size)
        .css_classes(["avatar"])
        .build();
    // No hexpand here: a child's expand flag leaks through the circle via
    // GTK's compute_expand, which would make the whole avatar an expanding
    // child of the header row (it then swallows half the row's spare width).
    let label = Label::builder()
        .label("π")
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    label.add_css_class("avatar-label");
    circle.append(&label);
    circle
}

/// Header row shown above assistant content: avatar, "pi", optional time.
/// A trailing spacer consumes all extra space so the group always sits at the
/// left edge regardless of how the parent distributes spare width.
fn assistant_header(time: Option<&str>) -> Box {
    let row = Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .valign(Align::Start)
        .build();

    row.append(&avatar(28));

    let name = Label::builder()
        .label("pi")
        .valign(Align::Center)
        .halign(Align::Start)
        .build();
    name.add_css_class("assistant-name");
    row.append(&name);

    if let Some(time) = time.filter(|t| !t.is_empty()) {
        let time = Label::builder()
            .label(format!("· {time}"))
            .valign(Align::Center)
            .halign(Align::Start)
            .build();
        time.add_css_class("msg-time");
        row.append(&time);
    }

    row.append(&spacer());
    row
}

/// An invisible box that absorbs all spare horizontal space.
fn spacer() -> Box {
    Box::builder().hexpand(true).halign(Align::Fill).build()
}

/// Collapsible reasoning ("thinking") block: a toggle row labeled "Reasoning"
/// with a rotating chevron, revealing a dimmed italic body.
pub struct ReasoningBox {
    pub container: Box,
    body: Label,
    revealer: Revealer,
    chevron: Image,
}

impl ReasoningBox {
    pub fn new() -> Self {
        let container = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .build();

        let toggle = ToggleButton::builder()
            .css_classes(["flat", "reasoning-toggle"])
            .halign(Align::Start)
            .build();
        toggle.set_active(false);

        let toggle_row = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .build();
        let chevron = Image::builder().icon_name("pan-end-symbolic").build();
        let label = Label::builder().label("Reasoning").build();
        label.add_css_class("reasoning-toggle-label");
        toggle_row.append(&chevron);
        toggle_row.append(&label);
        toggle.set_child(Some(&toggle_row));

        let body = Label::builder()
            .label("")
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(78)
            .hexpand(true)
            .build();
        body.add_css_class("reasoning-body");

        let revealer = Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .reveal_child(false)
            .child(&body)
            .build();

        let chevron_clone = chevron.clone();
        let revealer_clone = revealer.clone();
        toggle.connect_toggled(move |btn| {
            let expanded = btn.is_active();
            revealer_clone.set_reveal_child(expanded);
            chevron_clone
            .set_icon_name(Some(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" }));
        });

        container.append(&toggle);
        container.append(&revealer);

        Self { container, body, revealer, chevron }
    }

    pub fn set_text(&self, text: &str) {
        self.body.set_text(text);
        self.container.set_visible(!text.is_empty());
    }

    /// Append streaming reasoning text; keeps the block expanded while it
    /// grows.
    pub fn append(&self, text: &str) {
        let current = self.body.text().to_string();
        self.body.set_text(&format!("{current}{text}"));
        self.container.set_visible(true);
        self.expand();
    }

    pub fn expand(&self) {
        self.set_expanded(true);
    }

    pub fn collapse(&self) {
        self.set_expanded(false);
    }

    fn set_expanded(&self, expanded: bool) {
        if let Some(toggle) = self
            .container
            .first_child()
            .and_then(|c| c.downcast::<ToggleButton>().ok())
        {
            if toggle.is_active() != expanded {
                toggle.set_active(expanded);
            }
        }
        self.revealer.set_reveal_child(expanded);
        self.chevron
            .set_icon_name(Some(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" }));
    }
}

/// Build a user message row: right-aligned tinted bubble with the time, and
/// (when `on_edit` is given) a pencil button revealed on hover that lets the
/// message text be re-sent.
pub fn user_message(
    text: &str,
    time: Option<&str>,
    on_edit: Option<std::rc::Rc<dyn Fn()>>,
) -> MessageBubble {
    let container = Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .valign(Align::Start)
        .css_classes(["user-row"])
        .build();

    let bubble_row = Box::builder()
        .orientation(Orientation::Horizontal)
        .hexpand(true)
        .build();

    // Leading spacer pushes the bubble to the right edge no matter how the
    // parent Box distributes spare width.
    bubble_row.append(&spacer());

    let bubble = Box::builder()
        .orientation(Orientation::Vertical)
        .valign(Align::Start)
        .css_classes(["bubble-user"])
        .build();

    let label = Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(66)
        .build();
    label.add_css_class("message-text");

    bubble.append(&label);
    bubble_row.append(&bubble);
    container.append(&bubble_row);

    if let Some(on_edit) = on_edit {
        // Pencil button left of the bubble; visible only while hovering.
        let edit_btn = Button::builder()
            .icon_name("document-edit-symbolic")
            .css_classes(["flat", "msg-edit-btn"])
            .valign(Align::Center)
            .margin_end(6)
            .tooltip_text("Edit message")
            .build();
        edit_btn.set_visible(false);
        edit_btn.connect_clicked(move |_| on_edit());
        bubble_row.append(&edit_btn);

        let hover_btn = edit_btn.clone();
        let controller = gtk4::EventControllerMotion::new();
        controller.connect_enter(move |_, _, _| hover_btn.set_visible(true));
        let leave_btn = edit_btn.clone();
        controller.connect_leave(move |_| leave_btn.set_visible(false));
        bubble_row.add_controller(controller);
    }

    if let Some(time) = time.filter(|t| !t.is_empty()) {
        let time = Label::builder()
            .label(time)
            .halign(Align::End)
            .margin_end(4)
            .build();
        time.add_css_class("msg-time");
        container.append(&time);
    }

    MessageBubble { box_: container }
}

/// Build an assistant message row: avatar + "pi" + time header, optional
/// collapsed reasoning block, and selectable answer text.
pub fn assistant_message(text: &str, thinking: Option<&str>, time: Option<&str>) -> MessageBubble {
    let container = Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .valign(Align::Start)
        .spacing(4)
        .margin_start(2)
        .css_classes(["assistant-row"])
        .build();

    container.append(&assistant_header(time));

    if let Some(thinking) = thinking.filter(|t| !t.is_empty()) {
        let reasoning = ReasoningBox::new();
        reasoning.set_text(thinking);
        container.append(&reasoning.container);
    }

    if !text.is_empty() {
        let label = Label::builder()
            .label(text)
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(78)
            .hexpand(true)
            .selectable(true)
            .build();
        label.add_css_class("message-text");
        container.append(&label);
    }

    MessageBubble { box_: container }
}

/// Build a persistent error row shown when the provider gives up (all retries
/// exhausted) — plain toasts vanish, this stays in the transcript.
pub fn error_message(text: &str) -> MessageBubble {
    let container = Box::builder()
        .orientation(Orientation::Horizontal)
        .hexpand(true)
        .valign(Align::Start)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .css_classes(["error-row"])
        .build();

    let icon = Image::builder().icon_name("dialog-warning-symbolic").build();
    icon.add_css_class("error-icon");
    container.append(&icon);

    let label = Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(78)
        .hexpand(true)
        .selectable(true)
        .build();
    label.add_css_class("error-text");
    container.append(&label);

    MessageBubble { box_: container }
}

/// A streaming assistant row: header, animated status line, live reasoning
/// block, and the accumulating answer text. The status line disappears once
/// answer text starts arriving, and the reasoning block collapses then too.
pub struct StreamingBubble {
    pub container: Box,
    pub label: Label,
    pub reasoning: ReasoningBox,
    status_row: Box,
    status_label: Label,
    saw_text: std::cell::Cell<bool>,
}

impl StreamingBubble {
    pub fn new() -> Self {
        let container = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .spacing(4)
            .margin_start(2)
            .css_classes(["assistant-row"])
            .build();

        container.append(&assistant_header(Some(&now_time())));

        let status_row = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .margin_top(6)
            .build();
        let spinner = Spinner::new();
        spinner.start();
        spinner.set_size_request(14, 14);
        let status_label = Label::builder().label("Thinking…").xalign(0.0).build();
        status_label.add_css_class("stream-status");
        status_row.append(&spinner);
        status_row.append(&status_label);

        let reasoning = ReasoningBox::new();
        reasoning.container.set_visible(false);

        let label = Label::builder()
            .label("")
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(78)
            .hexpand(true)
            .selectable(true)
            .build();
        label.add_css_class("message-text");

        container.append(&status_row);
        container.append(&reasoning.container);
        container.append(&label);

        Self {
            container,
            label,
            reasoning,
            status_row,
            status_label,
            saw_text: std::cell::Cell::new(false),
        }
    }

    fn text_started(&self) {
        if !self.saw_text.get() {
            self.saw_text.set(true);
            self.status_row.set_visible(false);
            self.reasoning.collapse();
        }
    }

    /// Show a persistent retry notice in the status line (the provider failed
    /// and pi is retrying — otherwise the UI would sit silently on
    /// "Thinking…").
    pub fn show_retry(&self, attempt: u32, reason: &str) {
        let text = if reason.is_empty() {
            format!("Retrying (attempt {attempt})…")
        } else {
            format!("Retrying (attempt {attempt}) — {reason}")
        };
        self.status_label.set_text(&text);
        self.status_row.set_visible(true);
    }

    /// The retry succeeded: back to the plain thinking indicator (or nothing
    /// if answer text already arrived).
    pub fn retry_done(&self) {
        if self.saw_text.get() {
            self.status_row.set_visible(false);
        } else {
            self.status_label.set_text("Thinking…".to_string().as_str());
            self.status_row.set_visible(true);
        }
    }

    pub fn append_text(&self, text: &str) {
        self.text_started();
        let current = self.label.text().to_string();
        self.label.set_text(&format!("{current}{text}"));
    }

    pub fn set_text(&self, text: &str) {
        if !text.is_empty() {
            self.text_started();
        }
        self.label.set_text(text);
    }

    pub fn append_thinking(&self, text: &str) {
        self.reasoning.append(text);
    }

    pub fn set_thinking(&self, text: &str) {
        self.reasoning.set_text(text);
    }
}

fn tool_icon(tool_name: &str) -> &'static str {
    let name = tool_name.to_lowercase();
    if name.contains("bash") || name.contains("shell") || name.contains("terminal") {
        "utilities-terminal-symbolic"
    } else if name.contains("edit") || name.contains("write") {
        "document-edit-symbolic"
    } else if name.contains("read") || name.contains("cat") {
        "document-open-symbolic"
    } else if name.contains("search") || name.contains("grep") || name.contains("glob") {
        "system-search-symbolic"
    } else if name.contains("ls") || name.contains("tree") {
        "folder-symbolic"
    } else {
        "system-run-symbolic"
    }
}

/// A tool-run card: clickable header (chevron, tool icon, name, arg summary,
/// running spinner / done marker) over a collapsible monospace result body.
/// Expanded while the tool runs; collapses when it finishes.
pub struct ToolBlock {
    pub box_: Box,
    content: Box,
    title: Label,
    chevron: Image,
    revealer: Revealer,
    spinner: Spinner,
    status_icon: Image,
}

impl ToolBlock {
    pub fn new(tool_name: &str, summary: &str) -> Self {
        let container = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .css_classes(["tool-card"])
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(34)
            .margin_end(8)
            .build();

        let header = ToggleButton::builder()
            .css_classes(["flat", "tool-header-btn"])
            .hexpand(true)
            .halign(Align::Fill)
            .active(true)
            .build();

        let header_row = Box::builder()
            .orientation(Orientation::Horizontal)
            .spacing(8)
            .hexpand(true)
            .build();

        let chevron = Image::builder().icon_name("pan-down-symbolic").build();
        chevron.add_css_class("tool-status-icon");

        let icon = Image::builder()
            .icon_name(tool_icon(tool_name))
            .pixel_size(16)
            .build();
        icon.add_css_class("tool-status-icon");

        let title = Label::builder().label(tool_name).valign(Align::Center).build();
        title.add_css_class("tool-name");

        header_row.append(&chevron);
        header_row.append(&icon);
        header_row.append(&title);

        if !summary.is_empty() {
            let summary = Label::builder()
                .label(summary)
                .valign(Align::Center)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .xalign(0.0)
                .build();
            summary.add_css_class("tool-summary");
            header_row.append(&summary);
        }

        let spinner = Spinner::new();
        spinner.start();
        spinner.set_size_request(14, 14);
        header_row.append(&spinner);

        let status_icon = Image::builder().icon_name("").build();
        status_icon.set_visible(false);

        header.set_child(Some(&header_row));

        let content = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .css_classes(["tool-body"])
            .build();

        let revealer = Revealer::builder()
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .reveal_child(true)
            .child(&content)
            .build();

        let chevron_clone = chevron.clone();
        let revealer_clone = revealer.clone();
        header.connect_toggled(move |btn| {
            let expanded = btn.is_active();
            revealer_clone.set_reveal_child(expanded);
            chevron_clone
            .set_icon_name(Some(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" }));
        });

        container.append(&header);
        container.append(&revealer);

        Self { box_: container, content, title, chevron, revealer, spinner, status_icon }
    }

    /// Replace the body with `text` (latest partial or final tool result).
    pub fn set_content(&self, text: &str) {
        let label = Label::builder()
            .label(text)
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(88)
            .hexpand(true)
            .build();
        label.add_css_class("tool-text");
        label.add_css_class("monospace");

        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        self.content.append(&label);
    }

    /// Mark the run finished: stop the spinner, show a result marker, and
    /// collapse the body (click the header to reopen).
    pub fn set_finished(&self, is_error: bool) {
        self.spinner.set_visible(false);
        if is_error {
            self.status_icon.set_icon_name(Some("dialog-warning-symbolic"));
            self.status_icon.add_css_class("tool-error");
            self.title.add_css_class("tool-error");
            self.box_.add_css_class("tool-error");
        } else {
            self.status_icon.set_icon_name(Some("object-select-symbolic"));
        }
        self.status_icon.set_visible(true);
        self.set_expanded(false);
    }

    fn set_expanded(&self, expanded: bool) {
        if let Some(toggle) = self
            .box_
            .first_child()
            .and_then(|c| c.downcast::<ToggleButton>().ok())
        {
            if toggle.is_active() != expanded {
                toggle.set_active(expanded);
            }
        }
        self.revealer.set_reveal_child(expanded);
        self.chevron
            .set_icon_name(Some(if expanded { "pan-down-symbolic" } else { "pan-end-symbolic" }));
    }
}

/// The scrollable chat transcript area.
pub struct ChatView {
    pub scrolled: ScrolledWindow,
    pub list: Box,
    pub overlay: ToastOverlay,
}

impl ChatView {
    pub fn new() -> Self {
        let list = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .spacing(0)
            .css_classes(["chat-list"])
            .build();

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&list)
            .build();
        scrolled.add_css_class("chat-scroll");

        let overlay = ToastOverlay::new();
        overlay.add_css_class("chat-scroll");

        Self { scrolled, list, overlay }
    }

    pub fn append(&self, widget: &impl IsA<gtk4::Widget>) {
        self.list.append(widget);
        self.scroll_to_bottom();
    }

    pub fn scroll_to_bottom(&self) {
        let adjustment = self.scrolled.vadjustment();
        adjustment.set_value(adjustment.upper() - adjustment.page_size());
    }

    pub fn clear(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
    }
}
