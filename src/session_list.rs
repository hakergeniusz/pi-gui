//! Sidebar session list following GNOME HIG sidebar patterns.

use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, ScrolledWindow};
use pango::EllipsizeMode;

use std::cell::RefCell;
use std::rc::Rc;

/// A clickable session row in the sidebar.
#[derive(Clone)]
pub struct SessionRow {
    pub button: Button,
    pub title: Label,
    pub id: String,
}

impl SessionRow {
    pub fn new(id: &str, name: &str) -> Self {
        let button = Button::builder()
            .hexpand(true)
            .valign(Align::Center)
            .css_classes(["flat", "session-row"])
            .halign(Align::Fill)
            .build();

        let row = Box::builder()
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        let icon = gtk4::Image::builder()
            .icon_name("document-symbolic")
            .pixel_size(16)
            .build();
        icon.add_css_class("dim-label");

        let title = Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(EllipsizeMode::End)
            .build();
        title.add_css_class("session-title");

        row.append(&icon);
        row.append(&title);
        button.set_child(Some(&row));

        Self {
            button,
            title,
            id: id.to_string(),
        }
    }

    pub fn set_active(&self, active: bool) {
        if active {
            self.button.add_css_class("active");
            self.button.add_css_class("suggested-action");
        } else {
            self.button.remove_css_class("active");
            self.button.remove_css_class("suggested-action");
        }
    }
}

/// The sidebar containing the session list and a "new session" button.
pub struct SessionList {
    pub scrolled: ScrolledWindow,
    pub list: Box,
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
    /// appending (initial list population). Returns the (possibly existing)
    /// row for an already-present id after renaming it.
    pub fn add_session(&self, id: &str, name: &str, at_top: bool) -> SessionRow {
        if let Some(row) = self.rows.borrow().iter().find(|r| r.id == id).cloned() {
            row.title.set_label(name);
            return row;
        }
        let row = SessionRow::new(id, name);
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
}
