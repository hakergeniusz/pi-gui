//! GNOME HIG / libadwaita CSS for the pi GUI.
//!
//! All colors come from libadwaita's palette variables (@window_bg_color,
//! @view_bg_color, @card_bg_color, @accent_color, …) so light/dark style and
//! the user's accent color are followed automatically.

use gtk4::CssProvider;
use gdk4::Display;

const CSS: &str = r#"
/* ================= Base ================= */
window {
    background-color: @window_bg_color;
}

/* ================= Sidebar ================= */
.sidebar {
    background-color: @sidebar_bg_color;
}

.sidebar-scroll {
    background-color: transparent;
}

.session-list {
    padding: 2px 6px 12px;
}

.session-row {
    border-radius: 8px;
    padding: 8px 10px;
}

.session-row:hover {
    background-color: alpha(@sidebar_fg_color, 0.07);
}

.session-row.active {
    background-color: @accent_bg_color;
    color: @accent_fg_color;
}

.session-row.active .session-icon {
    color: @accent_fg_color;
    opacity: 1;
}

.session-title {
    font-weight: 500;
}

.session-subtitle {
    font-size: 0.82em;
    opacity: 0.55;
}

.session-row.active .session-subtitle {
    opacity: 0.8;
}

.new-chat-row {
    border-radius: 999px;
    padding: 10px 16px;
    font-weight: 600;
    margin: 10px 8px 6px;
}

/* ================= Chat transcript ================= */
.chat-scroll {
    background-color: @view_bg_color;
}

.chat-list {
    padding: 16px 20px 6px;
}

/* --- user message: right-aligned tinted bubble --- */
.user-row {
    margin-top: 14px;
}

.bubble-user {
    background-color: alpha(@accent_color, 0.16);
    border-radius: 14px 14px 4px 14px;
    padding: 9px 14px;
}

.msg-time {
    font-size: 0.75em;
    opacity: 0.5;
}

/* --- assistant message: avatar + name + plain text --- */
.assistant-row {
    margin-top: 14px;
}

.avatar {
    border-radius: 999px;
    background-color: alpha(@accent_color, 0.16);
    color: @accent_color;
}

.avatar-label {
    font-weight: 700;
}

.assistant-name {
    font-weight: 700;
}

.message-text {
    line-height: 1.45;
}

/* --- streaming status --- */
.stream-status {
    color: alpha(@view_fg_color, 0.55);
    font-size: 0.9em;
}

/* --- collapsible reasoning --- */
.reasoning-toggle {
    border-radius: 8px;
    padding: 5px 10px;
    margin-top: 2px;
}

.reasoning-toggle:hover {
    background-color: alpha(@view_fg_color, 0.06);
}

.reasoning-toggle-label {
    font-size: 0.86em;
    font-weight: 600;
    color: alpha(@view_fg_color, 0.6);
}

.reasoning-body {
    color: alpha(@view_fg_color, 0.55);
    font-style: italic;
    font-size: 0.9em;
    line-height: 1.4;
}

/* ================= Tool cards ================= */
.tool-card {
    background-color: @card_bg_color;
    border: 1px solid alpha(@window_fg_color, 0.11);
    border-radius: 12px;
}

.tool-card.tool-error {
    border-color: alpha(@error_color, 0.55);
}

.tool-header-btn {
    border-radius: 12px;
    padding: 8px 12px;
}

.tool-name {
    font-weight: 600;
    font-size: 0.9em;
}

.tool-summary {
    font-size: 0.85em;
    opacity: 0.55;
}

.tool-status-icon {
    opacity: 0.5;
}

.tool-status-icon.tool-error {
    color: @error_color;
    opacity: 1;
}

.tool-body {
    padding: 0 12px 10px;
}

.tool-text {
    font-size: 0.85em;
    line-height: 1.45;
    color: alpha(@view_fg_color, 0.8);
}

/* ================= Input bar ================= */
.input-card {
    background-color: @card_bg_color;
    border: 1px solid alpha(@window_fg_color, 0.11);
    border-radius: 16px;
    padding: 6px 6px 6px 4px;
    margin: 6px 16px 16px;
}

.input-card:focus-within {
    border-color: alpha(@accent_color, 0.55);
}

.input-textview {
    background-color: transparent;
    color: @view_fg_color;
    font-size: 0.98em;
}

.input-placeholder {
    color: alpha(@view_fg_color, 0.4);
    font-size: 0.98em;
}

/* ================= Welcome ================= */
.welcome-avatar {
    border-radius: 999px;
    background-color: alpha(@accent_color, 0.14);
    color: @accent_color;
}

.welcome-avatar-label {
    font-size: 2.6em;
    font-weight: 700;
}

.welcome-title {
    font-weight: 800;
    font-size: 1.9em;
}

.welcome-subtitle {
    font-size: 1.02em;
    opacity: 0.65;
}
"#;

pub fn load_css() -> Result<(), Box<dyn std::error::Error>> {
    let provider = CssProvider::new();
    let _ = provider.load_from_string(CSS);

    gtk4::style_context_add_provider_for_display(
        &Display::default().expect("no default display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    Ok(())
}
