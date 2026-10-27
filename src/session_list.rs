//! Sidebar session list following GNOME HIG sidebar patterns: rounded
//! selection pills, icon + title + dim date subtitle per row.

use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Image, Label, Orientation, ScrolledWindow, Spinner};
use pango::EllipsizeMode;

use std::cell::RefCell;
use std::rc::Rc;

/// A clickable session row in the sidebar.
#[derive(Clone)]
pub struct SessionRow {
    pub button: Button,
    title: Label,
    #[allow(dead_code)] // kept so the row can update its date label later
    subtitle: Label,
    icon: Image,
    spinner: Spinner,
    id: String,
}

impl SessionRow {
    pub fn new(id: &str, name: &str, subtitle: &str) -> Self {
        let button = Button::builder()
            .hexpand(true)
            .valign(Align::Center)
            .css_classes(["flat", "session-row"])
            .halign(Align::Fill)
            .build();

        let row = Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .spacing(10)
            .build();

        let icon = Image::builder()
            .icon_name("text-x-generic-symbolic")
            .pixel_size(16)
            .valign(Align::Start)
            .margin_top(2)
            .build();
        icon.add_css_class("session-icon");

        // Swapped in for the icon while this session's agent is running.
        let spinner = Spinner::new();
        spinner.set_size_request(16, 16);
        spinner.set_valign(Align::Start);
        spinner.set_margin_top(2);
        spinner.set_visible(false);
        spinner.start();

        let texts = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .spacing(1)
            .build();

        let title = Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(EllipsizeMode::End)
            .build();
        title.add_css_class("session-title");

        let subtitle = Label::builder()
            .label(subtitle)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(EllipsizeMode::End)
            .build();
        subtitle.add_css_class("session-subtitle");

        texts.append(&title);
        texts.append(&subtitle);

        row.append(&icon);
        row.append(&spinner);
        row.append(&texts);
        button.set_child(Some(&row));

        Self {
            button,
            title,
            subtitle,
            icon,
            spinner,
            id: id.to_string(),
        }
    }

    pub fn set_active(&self, active: bool) {
        if active {
            self.button.add_css_class("active");
        } else {
            self.button.remove_css_class("active");
        }
    }

    /// Show a spinner instead of the file icon while the agent works.
    pub fn set_busy(&self, busy: bool) {
        self.icon.set_visible(!busy);
        self.spinner.set_visible(busy);
    }
}

/// The sidebar containing the session list.
pub struct SessionList {
    pub scrolled: ScrolledWindow,
    list: Box,
    rows: RefCell<Vec<SessionRow>>,
    on_selected: RefCell<Option<Rc<dyn Fn(&str)>>>,
}

impl SessionList {
    pub fn new() -> Self {
        let list = Box::builder()
            .orientation(Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .spacing(0)
            .css_classes(["session-list"])
            .build();

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .child(&list)
            .build();
        scrolled.add_css_class("sidebar-scroll");

        Self {
            scrolled,
            list,
            rows: RefCell::new(Vec::new()),
            on_selected: RefCell::new(None),
        }
    }

    /// Set the callback invoked when a session row is clicked. Applies to rows
    /// added later as well.
    pub fn set_on_selected(&self, callback: Rc<dyn Fn(&str)>) {
        *self.on_selected.borrow_mut() = Some(callback);
    }

    /// Add a session row; `at_top` prepends it (new chats) instead of
    /// appending (initial list population). An already-present id is renamed.
    pub fn add_session(&self, id: &str, name: &str, subtitle: &str, at_top: bool) -> SessionRow {
        if let Some(row) = self.rows.borrow().iter().find(|r| r.id == id).cloned() {
            row.title.set_label(name);
            return row;
        }
        let row = SessionRow::new(id, name, subtitle);
        if let Some(cb) = self.on_selected.borrow().clone() {
            let cb = cb.clone();
            let id = row.id.clone();
            row.button.connect_clicked(move |_| cb(&id));
        }
        if at_top {
            self.list.prepend(&row.button);
            self.rows.borrow_mut().insert(0, row.clone());
        } else {
            self.list.append(&row.button);
            self.rows.borrow_mut().push(row.clone());
        }
        row
    }

    pub fn rename_session(&self, id: &str, name: &str) {
        if let Some(row) = self.rows.borrow().iter().find(|r| r.id == id) {
            row.title.set_label(name);
        }
    }

    pub fn set_active(&self, id: &str) {
        for row in self.rows.borrow().iter() {
            row.set_active(row.id == id);
        }
    }

    /// Mark the session currently running the agent with a spinner.
    pub fn set_busy(&self, id: &str, busy: bool) {
        for row in self.rows.borrow().iter() {
            if row.id == id {
                row.set_busy(busy);
            }
        }
    }
}
