//! GNOME HIG / Adwaita-inspired CSS for the pi GUI.
//! Loaded into the application's default style provider on startup.

use gtk4::CssProvider;
use gdk4::Display;

const CSS: &str = r#"/* 
* {
    -gtk-icon-shadow: none;
    -gtk-icon-style: regular;
}

window {
    background-color: @theme_bg_color;
}

/* ---------- Header ---------- */
.title {
    font-weight: 600;
    font-size: 1.15em;
    color: @theme_fg_color;
}

.subtitle {
    font-size: 0.9em;
    color: @theme_fg_color;
    opacity: 0.7;
}

/* pill buttons in header / welcome */
.pill {
    border-radius: 1000px;
    padding: 6px 12px;
}

/* ---------- Sidebar ---------- */
.sidebar {
    background-color: @theme_sidebar_bg_color;
}

.session-list {
    padding-top: 6px;
}

.session-row {
    border-radius: 6px;
    margin: 2px 8px;
}

.session-row:hover {
    background-color: shade(@theme_sidebar_bg_color, 1.1);
}

.session-row.active {
    background-color: @theme_selected_bg_color;
    color: @theme_selected_fg_color;
}

.session-row.active .dim-label {
    opacity: 0.8;
}

.session-row.new-chat-row {
    font-weight: 500;
    margin-top: 8px;
}

.sidebar-separator {
    margin: 0;
}

/* ---------- Chat ---------- */
.chat-page {
    background-color: @theme_bg_color;
}

.chat-scroll {
    background-color: @theme_base_color;
}

.chat-list {
    padding-top: 8px;
}

/* message bubbles */
.bubble {
    border-radius: 12px;
    padding: 8px 12px;
}

.bubble.user {
    background-color: @theme_selected_bg_color;
    color: @theme_selected_fg_color;
}

.bubble.assistant {
    background-color: shade(@theme_base_color, 1.05);
    border: 1px solid alpha(@theme_border_color, 0.3);
    color: @theme_fg_color;
}

.bubble.streaming {
    background-color: shade(@theme_base_color, 1.04);
    border: 1px solid alpha(@theme_border_color, 0.2);
}

.message-text {
    line-height: 1.4;
}

/* assistant reasoning, shown dimmed above the answer */
.thinking-text {
    line-height: 1.4;
    opacity: 0.55;
    font-style: italic;
    font-size: 0.92em;
}

/* tool call blocks */
.tool-block {
    border-radius: 8px;
    background-color: alpha(@theme_fg_color, 0.05);
    margin: 4px 0;
}

.tool-header {
    font-size: 0.85em;
    opacity: 0.65;
}

.tool-title {
    font-weight: 500;
}

.tool-title.tool-error {
    color: @error_color;
}

.tool-content {
    margin-top: 4px;
}

.tool-text {
    font-size: 0.9em;
    line-height: 1.45;
}

/* ---------- Input ---------- */
.input-frame {
    margin: 0;
    box-shadow: none;
}

.chat-input {
    border-radius: 1000px;
    padding: 8px 14px;
    font-size: 0.95em;
    background-color: @theme_input_bg_color;
    border: 1px solid alpha(@theme_border_color, 0.5);
}

.chat-input:focus {
    border: 1px solid @theme_fg_color;
}

/* ---------- Welcome ---------- */
.welcome-icon {
    opacity: 0.5;
}

.welcome-title {
    font-weight: 600;
    font-size: 1.5em;
    color: @theme_fg_color;
}

.welcome-subtitle {
    font-size: 1.05em;
    color: @theme_fg_color;
    opacity: 0.7;
}

/* dim label */
.dim-label {
    opacity: 0.6;
}

/* ---------- toasts ---------- */
toast {
    border-radius: 6px;
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
