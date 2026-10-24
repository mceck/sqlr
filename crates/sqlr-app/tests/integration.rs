//! One executable for the integration tests, to share compilation and linking.

mod admin;
mod agents_ui;
#[path = "main.rs"]
mod cli;
mod docs;
mod icon;
mod keymap;
mod objects;
mod plan_visual;
mod prefs;
mod redis;
mod terminal;
mod transfer_wizard;
#[path = "support/visual.rs"]
mod visual;
mod welcome;
