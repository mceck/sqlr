//! The egui terminal panel: a header with lifecycle actions and a custom grid canvas.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, PointerButton, Rect, Sense, Stroke, pos2,
    vec2,
};

use crate::input::{MouseEvent, encode_events, encode_mouse};
use crate::screen::{CellView, MouseProtocolMode, ScreenSnapshot, TermColor};
use crate::session::{SessionConfig, TerminalSession, TerminalStatus};

/// How long a freshly shown canvas waits before starting the shell, in seconds of egui time.
/// The first frames of a panel can report a provisional width; a TUI spawned on it may then
/// miss the following SIGWINCH and keep drawing for the wrong size until a manual resize.
const START_SETTLE: f64 = 0.05;

/// The colours and font the application gives the terminal.
#[derive(Clone, Debug)]
pub struct TerminalTheme {
    pub background: Color32,
    pub foreground: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
    pub font_size: f32,
    /// The app's icon font and the glyphs for the header's buttons.
    pub icon_font: egui::FontId,
    pub restart_icon: &'static str,
    pub close_icon: &'static str,
    pub hide_icon: &'static str,
    pub shell_icon: &'static str,
}

impl TerminalTheme {
    pub fn font(&self) -> FontId {
        FontId::new(self.font_size, FontFamily::Monospace)
    }

    pub fn color(&self, color: TermColor) -> Color32 {
        match color {
            TermColor::Default => self.foreground,
            TermColor::Idx(i) => self.indexed(i),
            TermColor::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
        }
    }

    fn indexed(&self, index: u8) -> Color32 {
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let i = index - 16;
                let scale = |v: u8| {
                    if v == 0 { 0 } else { 55 + u16::from(v) * 40 }
                };
                Color32::from_rgb(
                    scale(i / 36) as u8,
                    scale((i % 36) / 6) as u8,
                    scale(i % 6) as u8,
                )
            }
            _ => {
                let level = 8 + (index - 232) * 10;
                Color32::from_gray(level)
            }
        }
    }

    /// The cell's foreground and background after reverse video.
    pub fn cell_colors(&self, cell: &CellView) -> (Color32, Color32) {
        let mut fg = self.color(cell.fg);
        let mut bg = if cell.bg == TermColor::Default {
            self.background
        } else {
            self.color(cell.bg)
        };
        if cell.inverse {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.dim {
            fg = fg.gamma_multiply(0.65);
        }
        (fg, bg)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalAction {
    NewTab,
    NewShellTab,
    SelectTab(u64),
    CloseTab(u64),
    CloseTabsLeft(u64),
    CloseTabsRight(u64),
    Restart,
    Hide,
    OpenSettings,
}

/// The bottom terminal panel. Its session starts lazily on the first open and keeps running
/// while the panel is hidden.
pub struct TerminalPanel {
    session: Option<TerminalSession>,
    config: SessionConfig,
    open: bool,
    focused: bool,
    selection: Option<Selection>,
    dragging: bool,
    scroll: usize,
    /// Fractional rows not applied yet, so a slow trackpad still scrolls the history.
    scroll_pending: f32,
    /// The button held down, for the motion reports a mouse-tracking application expects.
    mouse_down: Option<PointerButton>,
    last_mouse_cell: Option<(u16, u16)>,
    started_once: bool,
    bell_until: Option<Instant>,
    last_rect: Rect,
    last_cells: (u16, u16),
    /// The egui time when a fresh canvas may spawn its session, once the layout settles.
    start_at: Option<f64>,
    focus_next: bool,
}

impl Default for TerminalPanel {
    fn default() -> Self {
        Self {
            session: None,
            config: SessionConfig::default(),
            open: false,
            focused: false,
            selection: None,
            dragging: false,
            scroll: 0,
            scroll_pending: 0.0,
            mouse_down: None,
            last_mouse_cell: None,
            started_once: false,
            bell_until: None,
            last_rect: Rect::NOTHING,
            last_cells: (0, 0),
            start_at: None,
            focus_next: false,
        }
    }
}

impl TerminalPanel {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Whether a shell was started at least once.
    pub fn started(&self) -> bool {
        self.started_once
    }

    pub fn session(&self) -> Option<&TerminalSession> {
        self.session.as_ref()
    }

    pub fn status(&self) -> Option<TerminalStatus> {
        self.session.as_ref().map(TerminalSession::status)
    }

    pub fn open(&mut self) {
        self.open = true;
        self.focus_next = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.focused = false;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.focus_next = self.open;
        if !self.open {
            self.focused = false;
        }
    }

    /// Updates the settings used by the next Restart; a running session is not killed.
    pub fn set_config(&mut self, config: SessionConfig) {
        self.config = config;
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// The canvas rect of the last frame, for tests that need to point at the grid.
    #[doc(hidden)]
    pub fn canvas_rect(&self) -> Rect {
        self.last_rect
    }

    pub fn clear(&mut self) {
        if let Some(session) = &self.session {
            session.clear();
        }
    }

    pub fn kill(&mut self) {
        if let Some(session) = &self.session {
            session.kill();
        }
    }

    pub fn restart(&mut self, repaint: Arc<dyn Fn() + Send + Sync>) {
        if let Some(session) = self.session.take() {
            session.kill();
        }
        // Restart from the size the canvas has now: starting at the config default and
        // resizing in the same frame races the new child's first tty size query.
        let (cols, rows) = self.start_cells();
        self.start(repaint, rows, cols);
    }

    /// The last canvas grid as `(cols, rows)`, or the config default before the first draw.
    fn start_cells(&self) -> (u16, u16) {
        match self.last_cells {
            (0, _) | (_, 0) => (self.config.cols, self.config.rows),
            (cols, rows) => (cols, rows),
        }
    }

    fn start(&mut self, repaint: Arc<dyn Fn() + Send + Sync>, rows: u16, cols: u16) {
        self.started_once = true;
        self.start_at = None;
        self.scroll = 0;
        self.scroll_pending = 0.0;
        self.mouse_down = None;
        self.last_mouse_cell = None;
        self.selection = None;
        let config = SessionConfig {
            rows: rows.max(1),
            cols: cols.max(1),
            ..self.config.clone()
        };
        self.session = Some(TerminalSession::spawn(&config, repaint));
    }

    /// Draws the panel.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: &TerminalTheme,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) -> Vec<TerminalAction> {
        self.show_tabs(ui, theme, repaint, &[], false, false)
    }

    pub(crate) fn show_tabs(
        &mut self,
        ui: &mut egui::Ui,
        theme: &TerminalTheme,
        repaint: Arc<dyn Fn() + Send + Sync>,
        tabs: &[crate::tabs::TabView],
        reveal_active: bool,
        show_shell_button: bool,
    ) -> Vec<TerminalAction> {
        let mut actions = Vec::new();
        self.header(
            ui,
            theme,
            &mut actions,
            tabs,
            reveal_active,
            show_shell_button,
        );
        ui.add_space(4.0);
        let active_id = tabs.iter().find(|tab| tab.selected).map(|tab| tab.id);
        ui.push_id(("terminal_canvas", active_id), |ui| {
            self.canvas(ui, theme, repaint);
        });
        actions
    }

    fn header(
        &mut self,
        ui: &mut egui::Ui,
        theme: &TerminalTheme,
        actions: &mut Vec<TerminalAction>,
        tabs: &[crate::tabs::TabView],
        reveal_active: bool,
        show_shell_button: bool,
    ) {
        let status = self.status();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(egui::RichText::new(">_").monospace().strong());
            ui.label(egui::RichText::new("Terminal").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hide = egui::RichText::new(theme.hide_icon).font(theme.icon_font.clone());
                let hide = ui
                    .add(egui::Button::new(hide).small())
                    .on_hover_text("Hide the terminal (shells keep running)");
                hide.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Hide terminal")
                });
                if hide.clicked() {
                    actions.push(TerminalAction::Hide);
                }
                let restart = egui::RichText::new(theme.restart_icon).font(theme.icon_font.clone());
                let restart = ui
                    .add(egui::Button::new(restart).small())
                    .on_hover_text("Restart the shell");
                restart.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Restart")
                });
                if restart.clicked() {
                    actions.push(TerminalAction::Restart);
                }
                if show_shell_button {
                    let mut label = egui::text::LayoutJob::default();
                    let color = ui.visuals().text_color();
                    label.append(
                        "+ ",
                        0.0,
                        egui::TextFormat::simple(egui::FontId::proportional(13.0), color),
                    );
                    label.append(
                        theme.shell_icon,
                        0.0,
                        egui::TextFormat::simple(theme.icon_font.clone(), color),
                    );
                    let shell = ui
                        .add(egui::Button::new(label).small())
                        .on_hover_text("New shell terminal");
                    shell.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            "New shell terminal",
                        )
                    });
                    if shell.clicked() {
                        actions.push(TerminalAction::NewShellTab);
                    }
                }
                let new = ui.small_button("+").on_hover_text("New terminal");
                new.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New terminal")
                });
                if new.clicked() {
                    actions.push(TerminalAction::NewTab);
                }
                egui::ScrollArea::horizontal()
                    .id_salt("terminal_tab_strip")
                    .animated(false)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add_space(4.0);
                            for (index, tab) in tabs.iter().enumerate() {
                                ui.push_id(tab.id, |ui| {
                                    let title = &tab.name;
                                    let label = ui.selectable_label(tab.selected, title);
                                    if label.clicked() {
                                        actions.push(TerminalAction::SelectTab(tab.id));
                                    }
                                    let close = egui::RichText::new(theme.close_icon)
                                        .font(theme.icon_font.clone());
                                    let close = ui
                                        .add(egui::Button::new(close).small().frame(false))
                                        .on_hover_text(format!("Close {title}"));
                                    close.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            format!("Close {title}"),
                                        )
                                    });
                                    if close.clicked() {
                                        actions.push(TerminalAction::CloseTab(tab.id));
                                    }
                                    let response = label | close;
                                    response.context_menu(|ui| {
                                        if ui.button("Close").clicked() {
                                            actions.push(TerminalAction::CloseTab(tab.id));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                index > 0,
                                                egui::Button::new("Close tabs to the left"),
                                            )
                                            .clicked()
                                        {
                                            actions.push(TerminalAction::CloseTabsLeft(tab.id));
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                index + 1 < tabs.len(),
                                                egui::Button::new("Close tabs to the right"),
                                            )
                                            .clicked()
                                        {
                                            actions.push(TerminalAction::CloseTabsRight(tab.id));
                                            ui.close();
                                        }
                                    });
                                    if tab.selected && reveal_active {
                                        ui.scroll_to_rect(
                                            response.rect.expand2(vec2(4.0, 0.0)),
                                            Some(egui::Align::Center),
                                        );
                                    }
                                    ui.separator();
                                });
                            }
                            ui.add_space(4.0);
                        });
                    });
            });
        });
        if let Some(TerminalStatus::Exited { code }) = &status {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("The shell exited with code {code}."))
                        .small()
                        .weak(),
                );
                if ui.small_button("Restart").clicked() {
                    actions.push(TerminalAction::Restart);
                }
            });
        }
        if let Some(TerminalStatus::Failed { message }) = &status {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("Could not start the shell: {message}"))
                        .small()
                        .color(Color32::from_rgb(0xe0, 0x4f, 0x4f)),
                );
                if ui.small_button("Retry").clicked() {
                    actions.push(TerminalAction::Restart);
                }
                if ui.small_button("Terminal settings").clicked() {
                    actions.push(TerminalAction::OpenSettings);
                }
            });
        }
    }

    fn canvas(
        &mut self,
        ui: &mut egui::Ui,
        theme: &TerminalTheme,
        repaint: Arc<dyn Fn() + Send + Sync>,
    ) {
        let font = theme.font();
        let (char_width, row_height) = ui.ctx().fonts_mut(|fonts| {
            (
                fonts.glyph_width(&font, 'M').max(1.0),
                fonts.row_height(&font).max(1.0),
            )
        });
        let available = ui.available_size();
        let desired = vec2(
            available.x.max(char_width),
            available.y.max(row_height).max(120.0),
        );
        let (rect, response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
        self.last_rect = rect;
        let cols = (rect.width() / char_width).floor().max(1.0) as u16;
        let rows = (rect.height() / row_height).floor().max(1.0) as u16;
        if self.session.is_none() {
            // Spawn at the canvas size, not at the config default: the child reads the tty
            // size while it starts, and a resize in the same frame can lose the race. A
            // missed SIGWINCH leaves full-screen agents at 80 columns until a manual resize.
            // Wait for the panel layout to settle first, because the first frame can report
            // a provisional width; only then is the spawned size trustworthy.
            let now = ui.input(|i| i.time);
            let deadline = *self.start_at.get_or_insert(now + START_SETTLE);
            self.last_cells = (cols, rows);
            if now >= deadline {
                self.start(repaint, rows, cols);
            } else {
                tracing::debug!(
                    target: "sqlr_terminal",
                    available = ?available,
                    width = rect.width(),
                    height = rect.height(),
                    char_width,
                    row_height,
                    cols,
                    rows,
                    "terminal canvas measured"
                );
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f64(deadline - now));
            }
        } else if (cols, rows) != self.last_cells {
            self.last_cells = (cols, rows);
            if let Some(session) = &mut self.session {
                session.resize(rows, cols);
            }
        }
        if self.focus_next {
            self.focus_next = false;
            ui.memory_mut(|m| m.request_focus(response.id));
        }
        if response.clicked() || response.drag_started() {
            ui.memory_mut(|m| m.request_focus(response.id));
        }
        // egui's focus is the single source of truth. A palette, a dialog or an editor that
        // takes it must also stop the terminal from reading the same keys, even when no
        // click moved the focus.
        self.focused = ui.memory(|m| m.has_focus(response.id));
        if self.focused {
            // Arrows, Tab and Escape are shell input: egui must not use them to move the
            // focus to the editor or to the toolbar.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            });
        }
        // The canvas owns the keyboard while the shell starts too.
        let Some(session) = self.session.as_ref() else {
            return;
        };
        // The wheel belongs to the application when it enabled mouse reports; a full-screen
        // application without them gets arrow keys; otherwise it moves the scrollback.
        let mut snapshot = session.snapshot(self.scroll);
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
        if response.hovered() && scroll_delta != 0.0 {
            // Consume the scroll so the scroll areas under the panel do not move too.
            ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
            let rows = wheel_rows(&mut self.scroll_pending, scroll_delta, row_height);
            let shift = ui.input(|i| i.modifiers.shift);
            if rows != 0 {
                if snapshot.mouse_mode != MouseProtocolMode::None && !shift {
                    if let Some(pos) = response.hover_pos() {
                        let (row, col) = cell_at(
                            rect,
                            char_width,
                            row_height,
                            snapshot.cols,
                            snapshot.rows,
                            pos,
                        );
                        let modifiers = ui.input(|i| i.modifiers);
                        let mut bytes = Vec::new();
                        for _ in 0..rows.unsigned_abs() {
                            if let Some(report) = encode_mouse(
                                MouseEvent::Wheel { up: rows > 0 },
                                col + 1,
                                row + 1,
                                modifiers,
                                snapshot.mouse_encoding,
                            ) {
                                bytes.extend(report);
                            }
                        }
                        session.write(&bytes);
                    }
                } else if snapshot.alternate && !shift {
                    let key: &[u8] = match (rows > 0, snapshot.application_cursor) {
                        (true, true) => b"\x1bOA",
                        (true, false) => b"\x1b[A",
                        (false, true) => b"\x1bOB",
                        (false, false) => b"\x1b[B",
                    };
                    let mut bytes = Vec::new();
                    for _ in 0..rows.unsigned_abs() {
                        bytes.extend_from_slice(key);
                    }
                    session.write(&bytes);
                } else {
                    self.scroll = offset_by(self.scroll, rows, snapshot.scrollback_len);
                    snapshot = session.snapshot(self.scroll);
                }
            }
        }
        self.paint(ui, rect, &snapshot, theme, char_width, row_height);
        if session.emulator_bell() {
            self.bell_until = Some(Instant::now() + Duration::from_millis(120));
        }
        if self.bell_until.is_some_and(|until| Instant::now() < until) {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::ZERO,
                Stroke::new(2.0, theme.foreground),
                egui::StrokeKind::Inside,
            );
        }
        self.interactions(ui, &response, &snapshot, theme, char_width, row_height);
    }

    fn paint(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        snapshot: &ScreenSnapshot,
        theme: &TerminalTheme,
        char_width: f32,
        row_height: f32,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::ZERO, theme.background);
        let selection = self.selection.map(|s| {
            let (a, b) = if (s.anchor.0, s.anchor.1) <= (s.cursor.0, s.cursor.1) {
                (s.anchor, s.cursor)
            } else {
                (s.cursor, s.anchor)
            };
            (a, b)
        });
        for row in 0..snapshot.rows {
            for col in 0..snapshot.cols {
                let Some(cell) = snapshot.cell(row, col) else {
                    continue;
                };
                // The second half of a wide character belongs to the cell before it.
                if col > 0
                    && snapshot
                        .cell(row, col - 1)
                        .is_some_and(|previous| previous.wide)
                {
                    continue;
                }
                let (mut fg, mut bg) = theme.cell_colors(cell);
                if selection
                    .is_some_and(|(a, b)| (row, col) >= (a.0, a.1) && (row, col) <= (b.0, b.1))
                {
                    bg = theme.selection;
                }
                let origin = pos2(
                    rect.left() + f32::from(col) * char_width,
                    rect.top() + f32::from(row) * row_height,
                );
                let width = if cell.wide {
                    char_width * 2.0
                } else {
                    char_width
                };
                if bg != theme.background {
                    painter.rect_filled(
                        Rect::from_min_size(origin, vec2(width, row_height)),
                        CornerRadius::ZERO,
                        bg,
                    );
                }
                // An erased cell has a background but no glyph to draw.
                if cell.text.is_empty() {
                    continue;
                }
                let text = if cell.text == " " {
                    " "
                } else {
                    cell.text.as_str()
                };
                if cell.bold {
                    fg = fg.gamma_multiply(1.1);
                }
                painter.text(
                    origin + vec2(0.0, row_height * 0.5),
                    Align2::LEFT_CENTER,
                    text,
                    font_from(theme),
                    fg,
                );
                if cell.underline {
                    painter.line_segment(
                        [
                            origin + vec2(0.0, row_height - 1.0),
                            origin + vec2(width, row_height - 1.0),
                        ],
                        Stroke::new(1.0, fg),
                    );
                }
                if cell.italic {
                    painter.text(
                        origin + vec2(1.0, row_height * 0.5),
                        Align2::LEFT_CENTER,
                        text,
                        font_from(theme),
                        fg,
                    );
                }
            }
        }
        if snapshot.cursor_visible {
            let (row, col) = snapshot.cursor;
            let origin = pos2(
                rect.left() + f32::from(col) * char_width,
                rect.top() + f32::from(row) * row_height,
            );
            let cursor = Rect::from_min_size(origin, vec2(char_width, row_height));
            match snapshot.cursor_shape {
                crate::screen::CursorShape::Block => {
                    painter.rect_filled(
                        cursor,
                        CornerRadius::ZERO,
                        theme.cursor.gamma_multiply(0.55),
                    );
                }
                crate::screen::CursorShape::Underline => {
                    painter.line_segment(
                        [cursor.left_bottom(), cursor.right_bottom()],
                        Stroke::new(2.0, theme.cursor),
                    );
                }
                crate::screen::CursorShape::Bar => {
                    painter.line_segment(
                        [cursor.left_top(), cursor.left_bottom()],
                        Stroke::new(2.0, theme.cursor),
                    );
                }
            }
        }
    }

    fn interactions(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        snapshot: &ScreenSnapshot,
        theme: &TerminalTheme,
        char_width: f32,
        row_height: f32,
    ) {
        // Use the same cell size the canvas painted with. Averaging the rect over the
        // column count is not the same thing when the width is not an exact multiple.
        let cell = |pos: egui::Pos2| {
            cell_at(
                self.last_rect,
                char_width,
                row_height,
                snapshot.cols,
                snapshot.rows,
                pos,
            )
        };
        // Shift is the terminal's escape hatch: it selects text even while the application
        // asked for mouse reports.
        let report_mouse =
            snapshot.mouse_mode != MouseProtocolMode::None && !ui.input(|i| i.modifiers.shift);
        if report_mouse {
            self.dragging = false;
            self.report_mouse(ui, snapshot, char_width, row_height);
        } else {
            if response.drag_started()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let cell = cell(pos);
                self.selection = Some(Selection {
                    anchor: cell,
                    cursor: cell,
                });
                self.dragging = true;
            }
            if self.dragging
                && let Some(pos) = response.interact_pointer_pos()
                && let Some(selection) = &mut self.selection
            {
                selection.cursor = cell(pos);
            }
            if response.drag_stopped() {
                self.dragging = false;
            }
            if response.double_clicked()
                && let Some(pos) = response.interact_pointer_pos()
            {
                let (row, col) = cell(pos);
                let mut start = col;
                while start > 0
                    && snapshot
                        .cell(row, start - 1)
                        .is_some_and(|c| !c.text.trim().is_empty())
                {
                    start -= 1;
                }
                let mut end = col;
                while end + 1 < snapshot.cols
                    && snapshot
                        .cell(row, end + 1)
                        .is_some_and(|c| !c.text.trim().is_empty())
                {
                    end += 1;
                }
                self.selection = Some(Selection {
                    anchor: (row, start),
                    cursor: (row, end + 1),
                });
            }
        }

        let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
        if self.focused {
            let encoded = ui.input(|i| {
                encode_events(
                    &i.events,
                    snapshot.application_cursor,
                    snapshot.bracketed_paste,
                    snapshot.kitty_flags,
                    mac,
                )
            });
            if encoded.copy {
                self.copy(ui.ctx(), snapshot);
            }
            if !encoded.bytes.is_empty() {
                if let Some(session) = &self.session {
                    session.write(&encoded.bytes);
                }
                self.scroll = 0;
                self.scroll_pending = 0.0;
            }
        }
        let _ = theme;
    }

    fn copy(&self, ctx: &egui::Context, snapshot: &ScreenSnapshot) {
        let Some(selection) = self.selection else {
            return;
        };
        let text = snapshot.text_between(selection.anchor, selection.cursor);
        if !text.is_empty() {
            ctx.copy_text(text);
        }
    }

    /// Forwards clicks, drags and motion to an application that enabled mouse tracking.
    fn report_mouse(
        &mut self,
        ui: &egui::Ui,
        snapshot: &ScreenSnapshot,
        char_width: f32,
        row_height: f32,
    ) {
        let rect = self.last_rect;
        let mut bytes = Vec::new();
        ui.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        if *pressed {
                            if !rect.contains(*pos) {
                                continue;
                            }
                        } else if self.mouse_down != Some(*button) {
                            continue;
                        }
                        let (row, col) = cell_at(
                            rect,
                            char_width,
                            row_height,
                            snapshot.cols,
                            snapshot.rows,
                            *pos,
                        );
                        let action = if *pressed {
                            MouseEvent::Press(*button)
                        } else {
                            MouseEvent::Release(*button)
                        };
                        let Some(report) = encode_mouse(
                            action,
                            col + 1,
                            row + 1,
                            *modifiers,
                            snapshot.mouse_encoding,
                        ) else {
                            continue;
                        };
                        if *pressed {
                            self.mouse_down = Some(*button);
                        } else {
                            self.mouse_down = None;
                            if snapshot.mouse_mode == MouseProtocolMode::Press {
                                continue;
                            }
                        }
                        self.last_mouse_cell = Some((row, col));
                        bytes.extend(report);
                    }
                    egui::Event::PointerMoved(pos) => {
                        let report = match snapshot.mouse_mode {
                            MouseProtocolMode::AnyMotion => Some(self.mouse_down),
                            MouseProtocolMode::ButtonMotion if self.mouse_down.is_some() => {
                                Some(self.mouse_down)
                            }
                            _ => None,
                        };
                        let Some(button) = report else {
                            continue;
                        };
                        // A drag that leaves the grid keeps reporting at its edge; a bare
                        // pointer outside the panel reports nothing.
                        if !rect.contains(*pos) && self.mouse_down.is_none() {
                            continue;
                        }
                        let (row, col) = cell_at(
                            rect,
                            char_width,
                            row_height,
                            snapshot.cols,
                            snapshot.rows,
                            *pos,
                        );
                        // Applications react per cell, not per pixel.
                        if self.last_mouse_cell == Some((row, col)) {
                            continue;
                        }
                        self.last_mouse_cell = Some((row, col));
                        if let Some(report) = encode_mouse(
                            MouseEvent::Move(button),
                            col + 1,
                            row + 1,
                            i.modifiers,
                            snapshot.mouse_encoding,
                        ) {
                            bytes.extend(report);
                        }
                    }
                    egui::Event::PointerGone => {
                        let Some(button) = self.mouse_down.take() else {
                            continue;
                        };
                        self.last_mouse_cell = None;
                        if snapshot.mouse_mode == MouseProtocolMode::Press {
                            continue;
                        }
                        if let Some(report) = encode_mouse(
                            MouseEvent::Release(button),
                            1,
                            1,
                            i.modifiers,
                            snapshot.mouse_encoding,
                        ) {
                            bytes.extend(report);
                        }
                    }
                    _ => {}
                }
            }
        });
        if !bytes.is_empty()
            && let Some(session) = &self.session
        {
            session.write(&bytes);
        }
    }
}

fn font_from(theme: &TerminalTheme) -> FontId {
    theme.font()
}

/// Moves the scrollback offset by a wheel delta in points. A positive delta moves the content
/// down, i.e. towards older lines, so the offset grows.
fn offset_by(current: usize, rows: i32, max: usize) -> usize {
    (current as i64 + i64::from(rows)).clamp(0, max as i64) as usize
}

/// Whole rows from a wheel delta in points. Fractions stay pending, so a slow trackpad
/// scrolls one row after a few frames instead of rounding every frame away.
fn wheel_rows(pending: &mut f32, delta: f32, row_height: f32) -> i32 {
    *pending += delta / row_height.max(1.0);
    let rows = pending.trunc();
    *pending -= rows;
    rows as i32
}

/// The 0-based cell under a point, clamped to the grid.
fn cell_at(
    rect: Rect,
    char_width: f32,
    row_height: f32,
    cols: u16,
    rows: u16,
    pos: egui::Pos2,
) -> (u16, u16) {
    let col = ((pos.x - rect.left()) / char_width)
        .floor()
        .clamp(0.0, f32::from(cols.saturating_sub(1))) as u16;
    let row = ((pos.y - rect.top()) / row_height)
        .floor()
        .clamp(0.0, f32::from(rows.saturating_sub(1))) as u16;
    (row, col)
}

impl TerminalSession {
    pub(crate) fn emulator_bell(&self) -> bool {
        self.take_bell()
    }
}

impl std::fmt::Debug for TerminalPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPanel")
            .field("open", &self.open)
            .field("focused", &self.focused)
            .field("started", &self.started_once)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> TerminalTheme {
        TerminalTheme {
            background: Color32::BLACK,
            foreground: Color32::WHITE,
            cursor: Color32::WHITE,
            selection: Color32::BLUE,
            ansi: [Color32::BLACK; 16],
            font_size: 13.0,
            icon_font: egui::FontId::proportional(14.0),
            restart_icon: "R",
            close_icon: "X",
            hide_icon: "v",
            shell_icon: "S",
        }
    }

    #[test]
    fn restart_reuses_the_canvas_cell_size() {
        let mut panel = TerminalPanel {
            last_cells: (174, 8),
            ..Default::default()
        };
        assert_eq!(panel.start_cells(), (174, 8));
        panel.last_cells = (0, 0);
        panel.config.cols = 80;
        panel.config.rows = 24;
        assert_eq!(panel.start_cells(), (80, 24));
    }

    #[test]
    fn focused_canvas_sends_input_and_copy_uses_the_selected_screen_text() {
        use crate::session::{BackendFactory, BackendParts};
        use crate::shell::CommandSpec;
        use parking_lot::Mutex;
        use std::io::{Cursor, Write};

        #[derive(Clone, Default)]
        struct Output(Arc<Mutex<Vec<u8>>>, Arc<std::sync::atomic::AtomicBool>);
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl BackendFactory for Output {
            fn start(
                &self,
                _: &CommandSpec,
                _: u16,
                _: u16,
            ) -> Result<BackendParts, crate::session::PtyError> {
                let killed = self.1.clone();
                let stopped = self.1.clone();
                Ok(BackendParts {
                    reader: Box::new(Cursor::new(b"hello\r\n\x1b[1;3;4;31mworld\x1b[0m\x07")),
                    writer: Box::new(self.clone()),
                    resize: Box::new(|_, _| {}),
                    kill: Box::new(move || {
                        killed.store(true, std::sync::atomic::Ordering::SeqCst);
                    }),
                    wait: Box::new(move || {
                        while !stopped.load(std::sync::atomic::Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(0)
                    }),
                    pid: None,
                })
            }
        }
        let output = Output::default();
        let config = SessionConfig::default();
        let spec = crate::shell::resolve_current(&config.shell, &config.cwd);
        let session = TerminalSession::spawn_with(&output, &spec, &config, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.snapshot(0).cell(1, 0).is_none_or(|c| c.text != "w") {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let mut panel = TerminalPanel {
            session: Some(session),
            started_once: true,
            focus_next: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let frame = |panel: &mut TerminalPanel, events| {
            let mut result = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 300.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    panel.show(ui, &theme(), Arc::new(|| {}));
                },
            );
            result.textures_delta.clear();
            result
        };
        let _ = frame(&mut panel, vec![]);
        let _ = frame(&mut panel, vec![egui::Event::Text("echo ready".into())]);
        assert!(panel.focused());
        assert_eq!(&*output.0.lock(), b"echo ready");
        assert!(panel.canvas_rect().width() > 500.0);
        panel.selection = Some(Selection {
            anchor: (0, 0),
            cursor: (0, 5),
        });
        let snapshot = panel.session().unwrap().snapshot(0);
        let mut copied = ctx.run_ui(Default::default(), |ui| panel.copy(ui.ctx(), &snapshot));
        assert!(
            copied
                .platform_output
                .commands
                .iter()
                .any(|command| matches!(
                    command, egui::OutputCommand::CopyText(text) if text == "hello"
                ))
        );
        copied.textures_delta.clear();
        panel.clear();
        assert_eq!(
            panel
                .session()
                .unwrap()
                .snapshot(0)
                .cell(0, 0)
                .unwrap()
                .text,
            ""
        );
        panel.close();
        assert!(!panel.focused());
        panel.kill();
    }

    #[test]
    fn mouse_reports_use_the_painted_cell_width() {
        use crate::session::{BackendFactory, BackendParts};
        use crate::shell::CommandSpec;
        use parking_lot::Mutex;
        use std::io::{Cursor, Write};
        use std::sync::atomic::{AtomicBool, Ordering};

        #[derive(Clone, Default)]
        struct MouseBackend(Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>);
        impl Write for MouseBackend {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl BackendFactory for MouseBackend {
            fn start(
                &self,
                _: &CommandSpec,
                _: u16,
                _: u16,
            ) -> Result<BackendParts, crate::session::PtyError> {
                let killed = self.1.clone();
                let stopped = self.1.clone();
                Ok(BackendParts {
                    // SGR mouse reports, as a full-screen agent enables them.
                    reader: Box::new(Cursor::new(b"\x1b[?1000h\x1b[?1006h".to_vec())),
                    writer: Box::new(self.clone()),
                    resize: Box::new(|_, _| {}),
                    kill: Box::new(move || killed.store(true, Ordering::SeqCst)),
                    wait: Box::new(move || {
                        while !stopped.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(0)
                    }),
                    pid: None,
                })
            }
        }

        let backend = MouseBackend::default();
        let config = SessionConfig::default();
        let spec = crate::shell::resolve_current(&config.shell, &config.cwd);
        let session = TerminalSession::spawn_with(&backend, &spec, &config, Arc::new(|| {}));
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.snapshot(0).mouse_mode == MouseProtocolMode::None {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        let mut panel = TerminalPanel {
            session: Some(session),
            started_once: true,
            ..Default::default()
        };
        let theme = theme();
        let font = theme.font();
        let ctx = egui::Context::default();
        fn frame(
            ctx: &egui::Context,
            panel: &mut TerminalPanel,
            theme: &TerminalTheme,
            events: Vec<egui::Event>,
            width: f32,
        ) {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 300.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    panel.show(ui, theme, Arc::new(|| {}));
                },
            );
            output.textures_delta.clear();
        }
        let mut width = 640.0;
        frame(&ctx, &mut panel, &theme, vec![], width);
        let char_width = ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M').max(1.0));
        let row_height = ctx.fonts_mut(|fonts| fonts.row_height(&font).max(1.0));
        // The canvas width is rarely an exact multiple of the glyph width. Find a point
        // whose painted cell differs from the one an averaged width would report.
        let mut target = None;
        for step in 0..20 {
            let rect = panel.canvas_rect();
            let cols = (rect.width() / char_width).floor().max(1.0) as u16;
            let averaged = rect.width() / f32::from(cols.max(1));
            for col in 0..cols {
                let x = (f32::from(col) + 0.5) * char_width;
                let painted = (x / char_width).floor() as u16;
                let averaged = (x / averaged).floor() as u16;
                if painted != averaged {
                    target = Some((painted, x));
                    break;
                }
            }
            if target.is_some() {
                break;
            }
            width = 640.0 + (step as f32 + 1.0) * 0.25;
            frame(&ctx, &mut panel, &theme, vec![], width);
        }
        let (col, x) = target.expect("a canvas width with a partial last cell");
        let rect = panel.canvas_rect();
        let pos = pos2(rect.left() + x, rect.top() + row_height * 0.5);
        frame(
            &ctx,
            &mut panel,
            &theme,
            vec![egui::Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            width,
        );
        let expected = format!("\x1b[<0;{};1M", col + 1);
        assert_eq!(String::from_utf8_lossy(&backend.0.lock()), expected);
        panel.kill();
    }

    #[test]
    fn restarting_a_failed_shell_clears_selection_and_reports_the_failure() {
        struct FailedBackend;
        impl crate::session::BackendFactory for FailedBackend {
            fn start(
                &self,
                _: &crate::shell::CommandSpec,
                _: u16,
                _: u16,
            ) -> Result<crate::session::BackendParts, crate::session::PtyError> {
                Err(crate::session::PtyError::Backend(
                    "test backend unavailable".into(),
                ))
            }
        }
        let config = SessionConfig {
            shell: "exit 0".into(),
            ..Default::default()
        };
        let spec = crate::shell::resolve_current(&config.shell, &config.cwd);
        let mut panel = TerminalPanel {
            session: Some(TerminalSession::spawn_with(
                &FailedBackend,
                &spec,
                &config,
                Arc::new(|| {}),
            )),
            started_once: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            assert!(panel.show(ui, &theme(), Arc::new(|| {})).is_empty());
        });
        output.textures_delta.clear();
        assert_eq!(
            panel.status(),
            Some(TerminalStatus::Failed {
                message: "test backend unavailable".into()
            })
        );
        panel.set_config(config);
        panel.open();
        panel.scroll = 12;
        panel.selection = Some(Selection {
            anchor: (0, 0),
            cursor: (0, 3),
        });
        panel.restart(Arc::new(|| {}));
        assert!(panel.started());
        assert_eq!(panel.scroll, 0);
        assert_eq!(panel.selection, None);
        let deadline = Instant::now() + Duration::from_secs(3);
        while panel.status().is_some_and(|s| s.is_running()) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(panel.status(), Some(TerminalStatus::Exited { code: 0 }));
        panel.toggle();
        assert!(!panel.is_open());
        panel.toggle();
        assert!(panel.is_open());
    }

    #[test]
    fn colour_mapping_covers_ansi_256_and_truecolour() {
        let theme = theme();
        assert_eq!(theme.color(TermColor::Default), Color32::WHITE);
        assert_eq!(theme.color(TermColor::Idx(1)), Color32::BLACK);
        assert_eq!(theme.color(TermColor::Idx(16)), Color32::from_rgb(0, 0, 0));
        assert_eq!(
            theme.color(TermColor::Idx(231)),
            Color32::from_rgb(255, 255, 255)
        );
        assert_eq!(theme.color(TermColor::Idx(232)), Color32::from_gray(8));
        assert_eq!(
            theme.color(TermColor::Rgb(1, 2, 3)),
            Color32::from_rgb(1, 2, 3)
        );
    }

    #[test]
    fn erased_cells_keep_their_background_colour() {
        // A TUI that erases with a background colour leaves empty cells that still have
        // to be painted; only the glyph is missing.
        let mut theme = theme();
        theme.ansi[1] = Color32::from_rgb(0xff, 0x00, 0x00);
        let snapshot = ScreenSnapshot {
            rows: 1,
            cols: 1,
            cells: vec![CellView {
                text: String::new(),
                bg: TermColor::Idx(1),
                ..CellView::default()
            }],
            ..Default::default()
        };
        let panel = TerminalPanel::default();
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(100.0, 50.0))),
                ..Default::default()
            },
            |ui| {
                panel.paint(
                    ui,
                    Rect::from_min_size(egui::Pos2::ZERO, vec2(20.0, 18.0)),
                    &snapshot,
                    &theme,
                    20.0,
                    18.0,
                );
            },
        );
        output.textures_delta.clear();
        assert!(
            output.shapes.iter().any(|clipped| matches!(
                &clipped.shape,
                egui::Shape::Rect(rect) if rect.fill == theme.ansi[1]
            )),
            "the erased cell's background was not painted"
        );
    }

    #[test]
    fn reverse_swaps_the_cell_colours() {
        let theme = theme();
        let cell = CellView {
            fg: TermColor::Idx(1),
            bg: TermColor::Idx(2),
            inverse: true,
            ..CellView::default()
        };
        let (fg, bg) = theme.cell_colors(&cell);
        assert_eq!(fg, theme.ansi[2]);
        assert_eq!(bg, theme.ansi[1]);
    }

    #[test]
    fn panel_starts_closed() {
        let panel = TerminalPanel::default();
        assert!(!panel.is_open());
        assert!(!panel.started());
    }

    #[test]
    fn scrolling_up_moves_into_the_scrollback() {
        // A positive delta moves the content down: older lines, a larger offset.
        let mut pending = 0.0;
        let rows = wheel_rows(&mut pending, 36.0, 18.0);
        assert_eq!(rows, 2);
        assert_eq!(offset_by(0, rows, 50), 2);
        assert_eq!(offset_by(2, wheel_rows(&mut pending, -18.0, 18.0), 50), 1);
    }

    #[test]
    fn slow_trackpad_deltas_accumulate() {
        let mut pending = 0.0;
        assert_eq!(wheel_rows(&mut pending, 6.0, 18.0), 0);
        assert_eq!(wheel_rows(&mut pending, 6.0, 18.0), 0);
        assert_eq!(wheel_rows(&mut pending, 6.0, 18.0), 1);
    }

    #[test]
    fn scrollback_offset_stays_within_the_history() {
        assert_eq!(offset_by(45, 10, 50), 50);
        assert_eq!(offset_by(0, -5, 50), 0);
    }

    #[test]
    fn the_cell_under_a_point_is_clamped_to_the_grid() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(80.0, 36.0));
        assert_eq!(cell_at(rect, 8.0, 18.0, 10, 2, pos2(10.0, 20.0)), (0, 0));
        assert_eq!(cell_at(rect, 8.0, 18.0, 10, 2, pos2(25.0, 40.0)), (1, 1));
        assert_eq!(
            cell_at(rect, 8.0, 18.0, 10, 2, pos2(1_000.0, 1_000.0)),
            (1, 9)
        );
    }
}
