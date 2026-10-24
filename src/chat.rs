//! Chat view widgets following GNOME HIG / libadwaita styling.
//! Message bubbles look like native Adwaita chat bubbles (accent color for
//! the user, subtle card surface for the assistant).

use gtk4::prelude::*;
use gtk4::{Align, Image, Label, Orientation, ScrolledWindow, Spinner};
use libadwaita::ToastOverlay;

/// A single rendered chat message.
pub struct MessageBubble {
    pub box_: gtk4::Box,
}

/// Build a user message bubble (right aligned, accent background).
pub fn user_message(text: &str) -> MessageBubble {
    let container = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .hexpand(true)
        .valign(Align::Start)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(120)
        .margin_end(12)
        .build();

    let bubble = gtk4::Box::builder()
        .orientation(Orientation::Vertical)
        .valign(Align::Start)
        .hexpand(false)
        .css_classes(["bubble", "user"])
        .build();

    let label = Label::builder()
        .label(text)
        .xalign(1.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(72)
        .hexpand(true)
        .valign(Align::Start)
        .build();
    label.add_css_class("message-text");

    bubble.append(&label);
    container.append(&bubble);

    MessageBubble { box_: container }
}

/// Build an assistant message bubble (left aligned, card surface). `thinking`
/// is shown as a dimmed italic block above the answer when present.
pub fn assistant_message(text: &str, thinking: Option<&str>) -> MessageBubble {
    let container = gtk4::Box::builder()
        .orientation(Orientation::Horizontal)
        .hexpand(true)
        .valign(Align::Start)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(120)
        .build();

    let bubble = gtk4::Box::builder()
        .orientation(Orientation::Vertical)
        .valign(Align::Start)
        .hexpand(false)
        .css_classes(["bubble", "assistant"])
        .build();

    if let Some(thinking) = thinking.filter(|t| !t.is_empty()) {
        let think = Label::builder()
            .label(thinking)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(72)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        think.add_css_class("thinking-text");
        bubble.append(&think);
    }

    let label = Label::builder()
        .label(if text.is_empty() { "…" } else { text })
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .max_width_chars(72)
        .hexpand(true)
        .valign(Align::Start)
        .build();
    label.add_css_class("message-text");

    bubble.append(&label);
    container.append(&bubble);

    MessageBubble { box_: container }
}

/// A streaming assistant bubble: spinner, dimmed live thinking text, and the
/// accumulating answer text.
pub struct StreamingBubble {
    pub container: gtk4::Box,
    pub label: Label,
    pub thinking_label: Label,
}

impl StreamingBubble {
    pub fn new() -> Self {
        let container = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .valign(Align::Start)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(120)
            .build();

        let bubble = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .valign(Align::Start)
            .hexpand(false)
            .css_classes(["bubble", "assistant", "streaming"])
            .build();

        let row = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .valign(Align::Center)
            .margin_end(8)
            .build();

        let spinner = Spinner::new();
        spinner.start();
        spinner.set_size_request(16, 16);
        row.append(&spinner);

        let status = Label::builder()
            .label("Thinking…")
            .xalign(0.0)
            .hexpand(true)
            .build();
        status.add_css_class("dim-label");
        row.append(&status);

        let thinking_label = Label::builder()
            .label("")
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(72)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        thinking_label.add_css_class("thinking-text");
        thinking_label.set_visible(false);

        let label = Label::builder()
            .label("")
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(72)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        label.add_css_class("message-text");

        bubble.append(&row);
        bubble.append(&thinking_label);
        bubble.append(&label);
        container.append(&bubble);

        Self {
            container,
            label,
            thinking_label,
        }
    }

    pub fn append_text(&self, text: &str) {
        let current = self.label.text().to_string();
        self.label.set_text(&format!("{current}{text}"));
    }

    pub fn set_text(&self, text: &str) {
        self.label.set_text(text);
    }

    pub fn append_thinking(&self, text: &str) {
        let current = self.thinking_label.text().to_string();
        self.thinking_label.set_text(&format!("{current}{text}"));
        self.thinking_label.set_visible(true);
    }

    pub fn set_thinking(&self, text: &str) {
        self.thinking_label.set_text(text);
        self.thinking_label.set_visible(!text.is_empty());
    }
}

/// A tool-call block rendered inline between assistant messages.
pub struct ToolBlock {
    pub box_: gtk4::Box,
    content: gtk4::Box,
    title: Label,
}

impl ToolBlock {
    pub fn new(tool_name: &str) -> Self {
        let container = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .css_classes(["tool-block"])
            .margin_top(4)
            .margin_bottom(4)
            .margin_start(8)
            .margin_end(8)
            .build();

        let header = gtk4::Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .valign(Align::Center)
            .css_classes(["tool-header"])
            .margin_bottom(4)
            .build();

        let icon = Image::builder()
            .icon_name("tools-symbolic")
            .pixel_size(14)
            .build();
        icon.add_css_class("dim-label");

        let title = Label::builder()
            .label(tool_name)
            .xalign(0.0)
            .hexpand(true)
            .build();
        title.add_css_class("tool-title");

        header.append(&icon);
        header.append(&title);

        let content = gtk4::Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .css_classes(["tool-content"])
            .margin_start(6)
            .build();

        container.append(&header);
        container.append(&content);

        Self {
            box_: container,
            content,
            title,
        }
    }

    /// Replace the body with `text` (latest partial or final tool result).
    pub fn set_content(&self, text: &str) {
        let label = Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .hexpand(true)
            .valign(Align::Start)
            .build();
        label.add_css_class("tool-text");

        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        self.content.append(&label);
    }

    pub fn set_error(&self, is_error: bool) {
        if is_error {
            self.title.add_css_class("tool-error");
        } else {
            self.title.remove_css_class("tool-error");
        }
    }
}

/// The scrollable chat transcript area.
pub struct ChatView {
    pub scrolled: ScrolledWindow,
    pub list: gtk4::Box,
    pub overlay: ToastOverlay,
}

impl ChatView {
    pub fn new() -> Self {
        let list = gtk4::Box::builder()
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

        Self {
            scrolled,
            list,
            overlay,
        }
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
