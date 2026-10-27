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
        .label("Agent")
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

    /// Replace the toggle label with how long the agent thought, once the
    /// thinking part of the message finished.
    pub fn set_duration(&self, secs: u64) {
        let label = if secs == 1 { "Thought for 1s".to_string() } else { format!("Thought for {secs}s") };
        self.set_toggle_label(&label);
    }

    fn set_toggle_label(&self, label: &str) {
        if let Some(box_) = self
            .container
            .first_child()
            .and_then(|c| c.downcast::<ToggleButton>().ok())
            .and_then(|t| t.child())
            .and_then(|c| c.downcast::<Box>().ok())
        {
            if let Some(child) = box_.last_child().and_then(|c| c.downcast::<Label>().ok()) {
                child.set_label(label);
            }
        }
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

/// Build an assistant message row: optional "Agent" + time header, collapsed
/// reasoning block, and selectable answer text. `with_header` is false for
/// follow-up assistant rows inside the same turn so the header appears only
/// once under the user's message. `thought_secs` labels the reasoning toggle
/// when the thinking duration is known (live runs).
pub fn assistant_message(
    text: &str,
    thinking: Option<&str>,
    time: Option<&str>,
    with_header: bool,
    thought_secs: Option<u64>,
) -> MessageBubble {
    let container = Box::builder()
        .orientation(Orientation::Vertical)
        .hexpand(true)
        .valign(Align::Start)
        .spacing(4)
        .margin_start(2)
        .css_classes(["assistant-row"])
        .build();

    if with_header {
        container.append(&assistant_header(time));
    } else {
        container.set_margin_top(0);
    }

    if let Some(thinking) = thinking.filter(|t| !t.is_empty()) {
        let reasoning = ReasoningBox::new();
        reasoning.set_text(thinking);
        if let Some(secs) = thought_secs {
            reasoning.set_duration(secs);
        }
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

/// A streaming assistant row: optional header, animated status line, live
/// reasoning block, and the accumulating answer text. The status line
/// disappears once answer text starts arriving, and the reasoning block
/// collapses then too (its toggle label becomes "Thought for Ns").
pub struct StreamingBubble {
    pub container: Box,
    pub label: Label,
    pub reasoning: ReasoningBox,
    status_row: Box,
    status_label: Label,
    saw_text: std::cell::Cell<bool>,
    /// Whether this row drew the "Agent" header (first row of the turn).
    pub has_header: bool,
    thinking_started: std::cell::Cell<Option<std::time::Instant>>,
    /// How long the thinking phase took, once it finished.
    thought_secs: std::cell::Cell<Option<u64>>,
}

impl StreamingBubble {
    pub fn new(with_header: bool) -> Self {
        let container = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .valign(Align::Start)
            .spacing(4)
            .margin_start(2)
            .css_classes(["assistant-row"])
            .build();

        if with_header {
            container.append(&assistant_header(Some(&now_time())));
        } else {
            container.set_margin_top(0);
        }

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
            has_header: with_header,
            thinking_started: std::cell::Cell::new(None),
            thought_secs: std::cell::Cell::new(None),
        }
    }

    /// The thinking part of this message finished: label the reasoning toggle
    /// with how long it took, and remember it for the finalized bubble.
    pub fn finish_thinking(&self) {
        if let Some(started) = self.thinking_started.get() {
            let secs = started.elapsed().as_secs();
            self.thought_secs.set(Some(secs));
            self.reasoning.set_duration(secs);
        }
    }

    /// Measured thinking duration of this message, if it already finished.
    pub fn thought_secs(&self) -> Option<u64> {
        self.thought_secs.get()
    }

    fn text_started(&self) {
        if !self.saw_text.get() {
            self.saw_text.set(true);
            self.status_row.set_visible(false);
            self.reasoning.collapse();
            self.finish_thinking();
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
        if self.thinking_started.get().is_none() {
            self.thinking_started.set(Some(std::time::Instant::now()));
        }
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
/// Expanded while the tool runs; collapses when it finishes. Write/edit runs
/// embed the file content / diff in the body instead of just the result line.
pub struct ToolBlock {
    pub box_: Box,
    content: Box,
    title: Label,
    chevron: Image,
    revealer: Revealer,
    spinner: Spinner,
    status_icon: Image,
    /// Body carries file content / diff; later results get appended, not
    /// replacing it.
    embedded: std::cell::Cell<bool>,
}

/// Cap for embedded file contents so a huge write doesn't stall the UI.
const MAX_EMBED_LINES: usize = 200;
/// Cap for the LCS matrix sides; larger files fall back to plain content.
const MAX_LCS_INPUT: usize = 1500;
/// Cap on rendered diff rows before truncating with a notice.
const MAX_DIFF_ROWS: usize = 400;

/// A git-style diff line: `-` removed, `+` added, context plain, with the
/// line numbers it came from / goes to.
#[derive(Clone, Copy, PartialEq)]
pub enum DiffKind {
    Context,
    Old,
    New,
}

pub struct DiffLine {
    pub kind: DiffKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

impl DiffLine {
    fn render(&self) -> String {
        let old = self.old_no.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
        let new = self.new_no.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
        let sign = match self.kind {
            DiffKind::Context => " ",
            DiffKind::Old => "-",
            DiffKind::New => "+",
        };
        let text = self.text.lines().next().unwrap_or("");
        format!("{old} {new} {sign}{text}")
    }
}

/// Line diff of two blocks, numbered from `old_start`/`new_start` (1-based
/// file line numbers when known). Returns None when either side is too big
/// for the LCS matrix or the sides share no lines at all (fully different).
pub fn diff_numbered(
    old: &str,
    new: &str,
    old_start: u32,
    new_start: u32,
) -> Option<Vec<DiffLine>> {
    let a: Vec<String> = old.lines().map(str::to_string).collect();
    let b: Vec<String> = new.lines().map(str::to_string).collect();
    if a.len() > MAX_LCS_INPUT || b.len() > MAX_LCS_INPUT {
        return None;
    }
    // LCS length table.
    let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i][j + 1].max(table[i + 1][j])
            };
        }
    }
    if table[0][0] == 0 && !(a.is_empty() && b.is_empty()) {
        return None; // nothing in common — caller falls back to plain content
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(DiffLine {
                kind: DiffKind::Context,
                old_no: Some(old_start + i as u32),
                new_no: Some(new_start + j as u32),
                text: a[i].clone(),
            });
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(DiffLine {
                kind: DiffKind::Old,
                old_no: Some(old_start + i as u32),
                new_no: None,
                text: a[i].clone(),
            });
            i += 1;
        } else {
            out.push(DiffLine {
                kind: DiffKind::New,
                old_no: None,
                new_no: Some(new_start + j as u32),
                text: b[j].clone(),
            });
            j += 1;
        }
    }
    while i < a.len() {
        out.push(DiffLine {
            kind: DiffKind::Old,
            old_no: Some(old_start + i as u32),
            new_no: None,
            text: a[i].clone(),
        });
        i += 1;
    }
    while j < b.len() {
        out.push(DiffLine {
            kind: DiffKind::New,
            old_no: None,
            new_no: Some(new_start + j as u32),
            text: b[j].clone(),
        });
        j += 1;
    }
    Some(out)
}

/// Locate `needle` inside `file` and return the 1-based line it starts at.
pub fn find_block_line(file: &str, needle: &str) -> Option<u32> {
    let first = needle.lines().next()?;
    let needle_lines: Vec<&str> = needle.lines().collect();
    let file_lines: Vec<&str> = file.lines().collect();
    for (idx, line) in file_lines.iter().enumerate() {
        if line.trim_end() == first.trim_end()
            && file_lines[idx..].iter().zip(&needle_lines).all(|(f, n)| f.trim_end() == n.trim_end())
        {
            return Some(idx as u32 + 1);
        }
    }
    None
}

/// Truncate a block to `max` lines with a notice, for display.
pub fn cap_lines(text: &str, max: usize) -> String {
    let count = text.lines().count();
    if count <= max {
        return text.to_string();
    }
    let mut out: String = text.lines().take(max).collect::<Vec<_>>().join("\n");
    out.push_str(&format!("\n… (+{} more lines)", count - max));
    out
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

        Self {
            box_: container,
            content,
            title,
            chevron,
            revealer,
            spinner,
            status_icon,
            embedded: std::cell::Cell::new(false),
        }
    }

    /// Show the full content written by a write tool in the body; the final
    /// result line is appended below it instead of replacing it.
    pub fn set_embedded_text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let body = format!("{}\n\n{}", cap_lines(text, MAX_EMBED_LINES), "— wrote this file —");
        self.set_content(&body);
        self.embedded.set(true);
    }

    /// Render git-style diff rows (grouped runs of same-kind lines share one
    /// label; `-`/`+` signs and line numbers are baked into the text).
    pub fn set_embedded_diff(&self, rows: &[DiffLine]) {
        if rows.is_empty() {
            return;
        }
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let shown = &rows[..rows.len().min(MAX_DIFF_ROWS)];
        let mut run: Vec<&DiffLine> = Vec::new();
        for line in shown {
            if let Some(last) = run.last() {
                if last.kind != line.kind {
                    self.append_diff_run(&run);
                    run.clear();
                }
            }
            run.push(line);
        }
        self.append_diff_run(&run);
        if rows.len() > MAX_DIFF_ROWS {
            self.append_result(&format!("… (+{} more diff lines)", rows.len() - MAX_DIFF_ROWS));
        }
        self.embedded.set(true);
    }

    fn append_diff_run(&self, run: &[&DiffLine]) {
        if run.is_empty() {
            return;
        }
        let text = run.iter().map(|l| l.render()).collect::<Vec<_>>().join("\n");
        let label = Label::builder()
            .label(text)
            .xalign(0.0)
            .yalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .max_width_chars(96)
            .hexpand(true)
            .build();
        label.add_css_class("tool-text");
        label.add_css_class("monospace");
        label.add_css_class(match run[0].kind {
            DiffKind::Old => "diff-old",
            DiffKind::New => "diff-new",
            DiffKind::Context => "diff-ctx",
        });
        self.content.append(&label);
    }

    /// Whether the body already carries embedded content (append results).
    pub fn is_embedded(&self) -> bool {
        self.embedded.get()
    }

    /// Append a result line below embedded content without replacing it.
    pub fn append_result(&self, text: &str) {
        if text.is_empty() {
            return;
        }
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
        self.content.append(&label);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_numbers_context_and_changes() {
        let old = "fn answer() -> i32 {\n    2 + 2\n}\n\nfn old_helper() {\n    todo!()\n}\n";
        let new = "fn answer() -> i32 {\n    2 + 2\n}\n\n#[cfg(test)]\nmod tests {\n}\n";
        let rows = diff_numbered(old, new, 1, 1).expect("diff exists");
        let rendered: Vec<String> = rows.iter().map(|l| l.render()).collect();
        // Shared prefix keeps its line numbers on both sides, unmarked.
        assert_eq!(rendered[0], "   1    1  fn answer() -> i32 {");
        assert_eq!(rendered[1], "   2    2      2 + 2");
        // Removed lines carry only their old number with a minus sign.
        assert!(rendered.iter().any(|l| l == "   5      -fn old_helper() {"));
        // Added lines carry only their new number with a plus sign.
        assert!(rendered.iter().any(|l| l == "        5 +#[cfg(test)]"));
        // Context after the change renumbers on both sides.
        assert!(rows.last().unwrap().old_no.is_some() && rows.last().unwrap().new_no.is_some());
    }

    #[test]
    fn diff_returns_none_for_fully_different_blocks() {
        assert!(diff_numbered("a\nb\nc", "1\n2\n3", 1, 1).is_none());
    }

    #[test]
    fn block_line_finds_nested_position() {
        let file = "line one\nline two\nfn run() {\n    body();\n}\n";
        assert_eq!(find_block_line(file, "fn run() {\n    body();\n}"), Some(3));
        assert_eq!(find_block_line(file, "not present"), None);
    }
}
