//! Locks: the blocking tree (each session that blocks others, with the sessions waiting on
//! it indented below, recursively) and the list of every lock with what it waits for.

use std::collections::HashSet;

use egui::RichText;
use sqlr_core::LockInfo;

use super::{ROW, caption, cell, heading, row};
use crate::theme::{self, Spacing};

/// The blocking chains as `(depth, session)` rows: roots are sessions that block others
/// without waiting themselves; below each, the sessions waiting on it. A session waiting
/// on several appears under each; a cycle (a deadlock about to be broken) stops instead of
/// looping.
pub fn blocking_tree(locks: &[LockInfo]) -> Vec<(usize, String)> {
    // (blocker, waiter) pairs, in order of appearance.
    let mut edges: Vec<(&str, &str)> = Vec::new();
    for l in locks.iter().filter(|l| !l.granted) {
        for b in &l.blocked_by {
            if !edges.contains(&(b.as_str(), l.session.as_str())) {
                edges.push((b, &l.session));
            }
        }
    }
    let waiting: HashSet<&str> = edges.iter().map(|(_, w)| *w).collect();
    let mut roots: Vec<&str> = Vec::new();
    for (b, _) in &edges {
        if !waiting.contains(b) && !roots.contains(b) {
            roots.push(b);
        }
    }
    let mut out = Vec::new();
    fn walk<'a>(
        session: &'a str,
        depth: usize,
        edges: &[(&'a str, &'a str)],
        path: &mut Vec<&'a str>,
        out: &mut Vec<(usize, String)>,
    ) {
        out.push((depth, session.to_string()));
        path.push(session);
        for (_, waiter) in edges.iter().filter(|(b, _)| *b == session) {
            if !path.contains(waiter) {
                walk(waiter, depth + 1, edges, path, out);
            }
        }
        path.pop();
    }
    for root in roots {
        walk(root, 0, &edges, &mut Vec::new(), &mut out);
    }
    // A deadlock cycle has no blocker that is not itself waiting: start from its first
    // edge, so the cycle is shown instead of an empty "nobody waits" tree.
    for (blocker, _) in &edges {
        if !out.iter().any(|(_, session)| session.as_str() == *blocker) {
            walk(blocker, 0, &edges, &mut Vec::new(), &mut out);
        }
    }
    out
}

const W_SESSION: f32 = 110.0;
const W_MODE: f32 = 200.0;
const W_OBJECT: f32 = 240.0;
const W_STATUS: f32 = 80.0;
const W_BLOCKED: f32 = 160.0;

/// Draws the blocking tree and the lock list; returns a session clicked in the tree.
pub fn locks_view(ui: &mut egui::Ui, locks: &[LockInfo]) -> Option<String> {
    let mut clicked = None;
    let p = theme::palette(ui.ctx());
    let tree = blocking_tree(locks);
    ui.label(RichText::new("Blocking").strong());
    if tree.is_empty() {
        caption(ui, "No session waits for another");
    }
    let mut parents: Vec<String> = Vec::new();
    for (depth, session) in &tree {
        parents.truncate(*depth);
        let text = match parents.last() {
            None => format!("Session {session} is blocking"),
            Some(parent) => format!("{session} waits for {parent}"),
        };
        ui.horizontal(|ui| {
            ui.add_space(*depth as f32 * Spacing::XL);
            let color = if *depth == 0 { p.error } else { p.warning };
            if ui
                .selectable_label(false, RichText::new(text).color(color))
                .clicked()
            {
                clicked = Some(session.clone());
            }
        });
        parents.push(session.clone());
    }
    ui.add_space(Spacing::M);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        ui.label(RichText::new("Locks").strong());
        let n = locks.len();
        caption(
            ui,
            &format!("{n} {}", if n == 1 { "lock" } else { "locks" }),
        );
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        heading(ui, W_SESSION, "Session");
        heading(ui, W_MODE, "Mode");
        heading(ui, W_OBJECT, "Object");
        heading(ui, W_STATUS, "Status");
        heading(ui, W_BLOCKED, "Waits for");
    });
    ui.spacing_mut().item_spacing.y = 0.0;
    egui::ScrollArea::both()
        .id_salt("locks_list")
        .auto_shrink([false, false])
        .show_rows(ui, ROW, locks.len(), |ui, range| {
            for l in &locks[range] {
                let fill = (!l.granted).then(|| crate::grid::change_tint(p.warning));
                row(ui, fill, |ui| {
                    cell(ui, W_SESSION, format!("Session {}", l.session));
                    cell(ui, W_MODE, &l.mode);
                    cell(ui, W_OBJECT, l.object.as_deref().unwrap_or(""));
                    let status = if l.granted { "granted" } else { "waiting" };
                    cell(ui, W_STATUS, status);
                    let blockers: Vec<String> = l
                        .blocked_by
                        .iter()
                        .map(|s| format!("Session {s}"))
                        .collect();
                    cell(ui, W_BLOCKED, blockers.join(", "));
                });
            }
        });
    clicked
}
