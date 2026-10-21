//! The UI kit's controls for host apps (a self-hosting patch, not upstream): the web host's own
//! windows (users, sharing, branches, the file panel's lists) draw with these, so they look like
//! the rest of the app and pass the kit guards (`ui_kit_guard_tests`).
use crate::ui_kit;
use egui::{Response, Ui};

/// A labelled checkbox.
pub fn checkbox(ui: &mut Ui, label: &str, value: &mut bool) -> Response {
    ui_kit::checkbox(ui, label, value)
}

/// One of a few choices, side by side; true when the choice changed.
pub fn choice(ui: &mut Ui, selected: &mut usize, options: &[&str]) -> bool {
    ui_kit::segmented(ui, selected, options, Default::default())
}

/// A dropdown list; `selected` is the chosen option's index.
pub fn dropdown(ui: &mut Ui, id: &str, selected: &mut usize, options: &[&str]) -> Response {
    ui_kit::dropdown(ui, id, selected, options)
}

/// A section that opens and closes with the kit's chevron; `None` while closed.
pub fn section<R>(ui: &mut Ui, id: &str, title: &str, default_open: bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    ui_kit::section(ui, id, title, default_open, add)
}
