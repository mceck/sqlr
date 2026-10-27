//! Object editors (spec §8.4): one editor per object, with a toolbar, a name
//! line and sub-tabs (Columns, Indexes, Constraints, Foreign keys, Triggers, Partitions,
//! Definition, DDL, Data, Properties).
//!
//! The editor holds the loaded definition (`before`, `None` for an object being created)
//! and an edited copy (`after`). Pending changes are always derived from the two
//! ([`sqlr_core::diff_object`]), so undoing an edit in a form removes its change. Like every
//! widget it takes data in (DDL text, tables for pickers, the data page) and returns
//! [`ObjectEditorAction`]s; it never calls the engine.

mod columns;
mod constraints;
mod foreign_keys;
mod indexes;
mod pending;
mod properties;
mod routine;
mod triggers;
mod view;

pub use pending::{code_editor, highlighted, sql_view};

use std::sync::Arc;

use egui::{Color32, RichText, TextEdit};
use sqlr_core::{
    ObjectDef, ObjectEditing, ObjectKind, SchemaChange, SqlSyntax, SqlTemplate, TableDef, TableRef,
    column_order_changed, diff_object,
};

use crate::theme::{self, Spacing, icons};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectSubTab {
    Columns,
    Indexes,
    Constraints,
    ForeignKeys,
    Triggers,
    Partitions,
    /// Grants on the object per role: drawn by the caller (the privilege grid), shown when
    /// [`ObjectEditorState::permissions`] is set.
    Permissions,
    /// The form of a view, routine, sequence, index or trigger.
    Definition,
    Ddl,
    Data,
    Properties,
}

impl ObjectSubTab {
    pub fn label(self) -> &'static str {
        match self {
            ObjectSubTab::Columns => "Columns",
            ObjectSubTab::Indexes => "Indexes",
            ObjectSubTab::Constraints => "Constraints",
            ObjectSubTab::ForeignKeys => "Foreign keys",
            ObjectSubTab::Triggers => "Triggers",
            ObjectSubTab::Partitions => "Partitions",
            ObjectSubTab::Permissions => "Permissions",
            ObjectSubTab::Definition => "Definition",
            ObjectSubTab::Ddl => "DDL",
            ObjectSubTab::Data => "Data",
            ObjectSubTab::Properties => "Properties",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObjectEditorAction {
    /// Show the pending DDL in a dialog.
    Preview,
    Apply,
    /// Already applied to the state: `after` is `before` again.
    Revert,
    /// Load the definition again (pending edits are dropped).
    Refresh,
    /// Open the object's data in a query tab.
    OpenData,
    GenerateSql(SqlTemplate),
    /// The foreign key picker needs the catalog's tables.
    PickTable,
    /// The Data sub-tab is shown and its page was not asked for since the last (re)load.
    /// The caller runs the object's first page and draws it below the editor, with the
    /// result tabs' grid editing (see [`ObjectEditorState::data_shown`]).
    LoadData,
    /// The Permissions sub-tab is shown and its grants were not asked for since the last
    /// (re)load. The caller loads them and draws the privilege grid below the editor (see
    /// [`ObjectEditorState::permissions_shown`]).
    LoadPermissions,
    /// The Properties sub-tab of an object with statistics is shown and they were not
    /// asked for since the last (re)load; they come back in [`ObjectEditorContext::stats`].
    LoadStats,
}

/// What the editor shows besides its own state.
pub struct ObjectEditorContext<'a> {
    /// Tables of the catalog with their columns: foreign key targets, index tables.
    pub tables: &'a [(TableRef, Vec<String>)],
    /// The connection's syntax, for highlighting expressions and DDL.
    pub syntax: Arc<SqlSyntax>,
    /// The object's DDL as loaded.
    pub current_ddl: Option<&'a str>,
    /// The statements that apply the pending changes, or why there are none.
    pub pending_ddl: Option<Result<&'a [String], &'a str>>,
    /// Work in progress ("Applying…", "Loading…"): the toolbar is disabled.
    pub busy: Option<&'a str>,
    /// Why Apply is not possible (a read-only session).
    pub apply_blocked: Option<&'a str>,
    /// The last apply's outcome: `(text, is_error)`.
    pub message: Option<(&'a str, bool)>,
    /// The table's statistics, shown on Properties once loaded.
    pub stats: Option<&'a sqlr_core::TableStats>,
}

impl ObjectEditorContext<'_> {
    /// Columns of the catalog table `table` (matched by name, then schema when both have one).
    pub(crate) fn columns_of(&self, table: &TableRef) -> Option<&[String]> {
        self.tables
            .iter()
            .find(|(t, _)| {
                t.name == table.name
                    && (t.schema.is_none() || table.schema.is_none() || t.schema == table.schema)
            })
            .map(|(_, columns)| columns.as_slice())
    }
}

/// A problem with the edited definition; errors block Apply, warnings do not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub text: String,
    pub error: bool,
}

impl Problem {
    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }

    fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }
}

/// The derived pending changes of one `before` and `after` pair.
struct Pending {
    before: Option<ObjectDef>,
    after: ObjectDef,
    changes: Vec<SchemaChange>,
    count: usize,
}

pub struct ObjectEditorState {
    /// The definition as loaded; `None` while the object is being created.
    pub before: Option<ObjectDef>,
    pub after: ObjectDef,
    pub tab: ObjectSubTab,
    /// The selected column row.
    pub column: Option<usize>,
    /// Scrolls the column list to this row on the next frame.
    pub scroll_to_column: Option<usize>,
    pub index: Option<usize>,
    /// Index into `constraints` of the selected primary key, unique or check constraint.
    pub constraint: Option<usize>,
    /// Index into `constraints` of the selected foreign key.
    pub foreign_key: Option<usize>,
    pub trigger: Option<usize>,
    /// The partition being attached: table name and bound.
    pub(crate) attach: (String, String),
    pending: Option<Pending>,
    /// The problems of one `after` (and the type suggestions they were checked against).
    problems: Option<(ObjectDef, Vec<String>, Vec<Problem>)>,
    /// `LoadData` was asked for since the last (re)load.
    data_requested: bool,
    /// The driver administers grants on this kind: the Permissions sub-tab is shown.
    pub permissions: bool,
    /// The driver has table statistics: Properties asks for them.
    pub statistics: bool,
    /// `LoadPermissions` and `LoadStats` were asked for since the last (re)load.
    permissions_requested: bool,
    stats_requested: bool,
}

impl ObjectEditorState {
    /// An editor on a loaded object.
    pub fn new(def: ObjectDef) -> Self {
        let tab = Self::first_tab(&def);
        Self {
            before: Some(def.clone()),
            after: def,
            tab,
            column: None,
            scroll_to_column: None,
            index: None,
            constraint: None,
            foreign_key: None,
            trigger: None,
            attach: Default::default(),
            pending: None,
            problems: None,
            data_requested: false,
            permissions: false,
            statistics: false,
            permissions_requested: false,
            stats_requested: false,
        }
    }

    /// An editor on an object to create: Apply creates `def`.
    pub fn new_object(def: ObjectDef) -> Self {
        Self {
            before: None,
            ..Self::new(def)
        }
    }

    fn first_tab(def: &ObjectDef) -> ObjectSubTab {
        match def {
            ObjectDef::Table(_) => ObjectSubTab::Columns,
            ObjectDef::Schema(_) | ObjectDef::Database(_) | ObjectDef::Role(_) => {
                ObjectSubTab::Properties
            }
            _ => ObjectSubTab::Definition,
        }
    }

    pub fn is_new(&self) -> bool {
        self.before.is_none()
    }

    /// The Data sub-tab is the one shown: the caller draws the page below the editor.
    pub fn data_shown(&self) -> bool {
        self.tab == ObjectSubTab::Data && self.sub_tabs().contains(&ObjectSubTab::Data)
    }

    /// The Permissions sub-tab is the one shown: the caller draws the grid below the editor.
    pub fn permissions_shown(&self) -> bool {
        self.tab == ObjectSubTab::Permissions
            && self.sub_tabs().contains(&ObjectSubTab::Permissions)
    }

    pub fn kind(&self) -> ObjectKind {
        self.after.kind()
    }

    /// The object's name as edited.
    pub fn name(&self) -> String {
        self.after.object_ref().name
    }

    fn refresh_pending(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.after == self.after && p.before == self.before)
        {
            return;
        }
        let (changes, count) = match &self.before {
            None => (vec![SchemaChange::Create(self.after.clone())], 1),
            Some(before) => {
                let changes = diff_object(before, &self.after);
                let reorder = match (before, &self.after) {
                    (ObjectDef::Table(b), ObjectDef::Table(a)) => column_order_changed(b, a),
                    _ => false,
                };
                let mut count = changes.len() + usize::from(reorder);
                // A change the change list cannot name (SQLite rebuilds) still counts.
                if count == 0 && *before != self.after {
                    count = 1;
                }
                (changes, count)
            }
        };
        self.pending = Some(Pending {
            before: self.before.clone(),
            after: self.after.clone(),
            changes,
            count,
        });
    }

    /// The pending changes (for a new object, its creation).
    pub fn changes(&mut self) -> &[SchemaChange] {
        self.refresh_pending();
        self.pending.as_ref().map_or(&[], |p| &p.changes)
    }

    /// How many changes are pending (a column reorder counts as one).
    pub fn pending(&mut self) -> usize {
        self.refresh_pending();
        self.pending.as_ref().map_or(0, |p| p.count)
    }

    /// Drops every edit.
    pub fn revert(&mut self) {
        if let Some(before) = &self.before {
            self.after = before.clone();
        }
    }

    /// The object as now loaded (after Refresh or an apply): no edit is pending.
    pub fn reload(&mut self, def: ObjectDef) {
        self.before = Some(def.clone());
        self.after = def;
        self.data_requested = false;
        self.permissions_requested = false;
        self.stats_requested = false;
    }

    /// The sub-tabs this object shows, in order.
    pub fn sub_tabs(&self) -> Vec<ObjectSubTab> {
        let mut tabs = self.form_tabs();
        // Grants exist once the object does; they go right before the DDL.
        if self.permissions && !self.is_new() {
            let at = tabs
                .iter()
                .position(|t| *t == ObjectSubTab::Ddl)
                .unwrap_or(tabs.len());
            tabs.insert(at, ObjectSubTab::Permissions);
        }
        tabs
    }

    fn form_tabs(&self) -> Vec<ObjectSubTab> {
        use ObjectSubTab::*;
        let data = !self.is_new();
        match &self.after {
            ObjectDef::Table(t) => {
                let mut tabs = vec![Columns, Indexes, Constraints, ForeignKeys, Triggers];
                if !t.partitions.is_empty() || t.option("partition_by").is_some() {
                    tabs.push(Partitions);
                }
                tabs.push(Properties);
                tabs.push(Ddl);
                if data {
                    tabs.push(Data);
                }
                tabs
            }
            ObjectDef::View(_) => {
                let mut tabs = vec![Definition, Properties, Ddl];
                if data {
                    tabs.push(Data);
                }
                tabs
            }
            ObjectDef::Routine(_) => vec![Definition, Properties, Ddl],
            ObjectDef::Sequence(_) | ObjectDef::Index { .. } | ObjectDef::Trigger(_) => {
                vec![Definition, Ddl]
            }
            ObjectDef::Schema(_) | ObjectDef::Database(_) | ObjectDef::Role(_) => {
                vec![Properties, Ddl]
            }
        }
    }

    /// What is wrong with the edited definition; `column_types` are the driver's type
    /// suggestions (an unknown type is a warning). Cached until `after` changes.
    pub fn problems(&mut self, column_types: &[String]) -> Vec<Problem> {
        if let Some((after, types, problems)) = &self.problems
            && *after == self.after
            && types.as_slice() == column_types
        {
            return problems.clone();
        }
        let problems = self.check(column_types);
        self.problems = Some((self.after.clone(), column_types.to_vec(), problems.clone()));
        problems
    }

    fn check(&self, column_types: &[String]) -> Vec<Problem> {
        let mut out = Vec::new();
        if self.name().trim().is_empty() {
            out.push(Problem::error("The name is empty"));
        }
        match &self.after {
            ObjectDef::Table(t) => table_problems(t, column_types, &mut out),
            ObjectDef::View(v) if v.query.trim().is_empty() => {
                out.push(Problem::error("The view's query is empty"))
            }
            ObjectDef::Routine(r) if r.body.trim().is_empty() => {
                out.push(Problem::error("The body is empty"))
            }
            ObjectDef::Trigger(t) if t.body.trim().is_empty() => {
                out.push(Problem::error("The trigger's action is empty"))
            }
            ObjectDef::Trigger(t) if t.table.name.is_empty() => {
                out.push(Problem::error("Choose the trigger's table"))
            }
            ObjectDef::Index { table, index } => {
                if table.name.is_empty() {
                    out.push(Problem::error("Choose the index's table"));
                }
                if index.columns.is_empty() && index.definition.is_none() {
                    out.push(Problem::error("The index has no column"));
                }
            }
            _ => {}
        }
        out
    }
}

/// The base of a type name (`varchar(20)` → `varchar`), lowercase.
fn type_base(type_name: &str) -> String {
    type_name
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
}

/// `type_name` is one of the suggestions (or there are none).
pub(crate) fn known_type(type_name: &str, column_types: &[String]) -> bool {
    let base = type_base(type_name);
    column_types.is_empty()
        || base.ends_with("[]")
        || column_types.iter().any(|t| type_base(t) == base)
}

fn table_problems(t: &TableDef, column_types: &[String], out: &mut Vec<Problem>) {
    if t.columns.is_empty() {
        out.push(Problem::error("The table has no column"));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, c) in t.columns.iter().enumerate() {
        if c.name.trim().is_empty() {
            out.push(Problem::error(format!("Column {} has no name", i + 1)));
        } else if !seen.insert(c.name.as_str()) {
            out.push(Problem::error(format!(
                "Column \"{}\" appears twice",
                c.name
            )));
        }
        if c.type_name.trim().is_empty() {
            out.push(Problem::error(format!("Column \"{}\" has no type", c.name)));
        } else if !known_type(&c.type_name, column_types) {
            out.push(Problem::warning(format!(
                "Unknown type \"{}\" for column \"{}\"",
                c.type_name, c.name
            )));
        }
    }
    for c in &t.constraints {
        if let sqlr_core::ConstraintDef::ForeignKey {
            columns,
            target,
            target_columns,
            ..
        } = c
        {
            if target.name.is_empty() {
                out.push(Problem::error("A foreign key has no target table"));
            } else if columns.is_empty() || columns.iter().any(|c| c.is_empty()) {
                out.push(Problem::error("A foreign key has an empty column"));
            } else if target_columns.iter().any(|c| c.is_empty()) {
                out.push(Problem::error("A foreign key has an empty target column"));
            }
        }
    }
    for i in &t.indexes {
        if i.name.trim().is_empty() {
            out.push(Problem::error("An index has no name"));
        }
        if i.columns.is_empty() && i.definition.is_none() {
            out.push(Problem::error(format!(
                "Index \"{}\" has no column",
                i.name
            )));
        }
    }
}

/// The name field of `def`.
fn name_mut(def: &mut ObjectDef) -> &mut String {
    match def {
        ObjectDef::Database(d) => &mut d.name,
        ObjectDef::Schema(s) => &mut s.name,
        ObjectDef::Table(t) => &mut t.table.name,
        ObjectDef::View(v) => &mut v.view.name,
        ObjectDef::Sequence(s) => &mut s.sequence.name,
        ObjectDef::Routine(r) => &mut r.name,
        ObjectDef::Trigger(t) => &mut t.name,
        ObjectDef::Index { index, .. } => &mut index.name,
        ObjectDef::Role(r) => &mut r.name,
    }
}

/// The explorer icon of an object kind and its color.
pub fn kind_icon(kind: ObjectKind) -> (&'static str, fn(&theme::Palette) -> Color32) {
    use sqlr_core::NodeKind;
    let node = match kind {
        ObjectKind::Database => NodeKind::Database,
        ObjectKind::Schema => NodeKind::Schema,
        ObjectKind::Table => NodeKind::Table,
        ObjectKind::View => NodeKind::View,
        ObjectKind::MaterializedView => NodeKind::MaterializedView,
        ObjectKind::Index => NodeKind::Index,
        ObjectKind::Sequence => NodeKind::Sequence,
        ObjectKind::Function => NodeKind::Function,
        ObjectKind::Procedure => NodeKind::Procedure,
        ObjectKind::Trigger => NodeKind::Trigger,
        ObjectKind::Role => NodeKind::Schema,
    };
    icons::for_node(node)
}

/// The "Generate SQL" templates that apply to an object of `kind`.
pub fn templates_for(kind: ObjectKind) -> Vec<SqlTemplate> {
    use SqlTemplate::*;
    match kind {
        ObjectKind::Table => vec![Select, Insert, Update, Delete, Create, Drop],
        ObjectKind::View => vec![Select, Insert, Update, Delete, Create, Drop],
        ObjectKind::MaterializedView => vec![Select, Create, Drop],
        _ => vec![Create, Drop],
    }
}

/// The menu label of a template.
pub fn template_label(template: SqlTemplate) -> &'static str {
    match template {
        SqlTemplate::Select => "SELECT",
        SqlTemplate::Insert => "INSERT",
        SqlTemplate::Update => "UPDATE",
        SqlTemplate::Delete => "DELETE",
        SqlTemplate::Create => "CREATE",
        SqlTemplate::Drop => "DROP",
    }
}

/// Edits an optional text: empty means `None`.
pub(crate) fn optional_text(
    ui: &mut egui::Ui,
    value: &mut Option<String>,
    width: f32,
    hint: &str,
) -> egui::Response {
    let mut text = value.clone().unwrap_or_default();
    let response = crate::text_menu::text_edit(
        ui,
        TextEdit::singleline(&mut text)
            .desired_width(width)
            .hint_text(hint),
    );
    if response.changed() {
        *value = (!text.is_empty()).then_some(text);
    }
    response
}

/// A single-line expression field: monospace, highlighted with the connection's syntax.
/// Its text is written into the DDL as typed (labelled "expression" through the hint).
pub(crate) fn expr_field(
    ui: &mut egui::Ui,
    value: &mut Option<String>,
    syntax: &SqlSyntax,
    width: f32,
    hint: &str,
) -> egui::Response {
    let mut text = value.clone().unwrap_or_default();
    let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
        let mut job = highlighted(ui.ctx(), syntax, buf.as_str());
        job.wrap.max_width = wrap;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let response = crate::text_menu::text_edit(
        ui,
        TextEdit::singleline(&mut text)
            .desired_width(width)
            .hint_text(hint)
            .font(theme::monospace(ui.ctx()))
            .layouter(&mut layouter),
    );
    if response.changed() {
        *value = (!text.is_empty()).then_some(text);
    }
    response
}

/// A caption in the muted color, above or beside a field.
pub(crate) fn caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).color(theme::palette(ui.ctx()).text_muted))
}

/// A list of names on the left (selectable) and `detail` for the selected one on the right.
pub(crate) fn master_detail(
    ui: &mut egui::Ui,
    id: &str,
    names: &[String],
    selected: &mut Option<usize>,
    empty: &str,
    detail: impl FnOnce(&mut egui::Ui, usize),
) {
    if selected.is_some_and(|i| i >= names.len()) {
        *selected = None;
    }
    if selected.is_none() && !names.is_empty() {
        *selected = Some(0);
    }
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(220.0);
            egui::ScrollArea::vertical()
                .id_salt(("object_list", id))
                .auto_shrink([false, true])
                .max_height(ui.available_height())
                .show(ui, |ui| {
                    if names.is_empty() {
                        caption(ui, empty);
                    }
                    for (i, name) in names.iter().enumerate() {
                        if ui.selectable_label(*selected == Some(i), name).clicked() {
                            *selected = Some(i);
                        }
                    }
                });
        });
        ui.separator();
        ui.vertical(|ui| {
            if let Some(i) = *selected {
                egui::ScrollArea::vertical()
                    .id_salt(("object_detail", id))
                    .auto_shrink([false, true])
                    .show(ui, |ui| detail(ui, i));
            }
        });
    });
}

/// A two-column form grid.
pub(crate) fn form_grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(("object_form", id))
        .num_columns(2)
        .spacing([Spacing::L, Spacing::S])
        .show(ui, add);
}

/// Draws the editor and returns what the user asked for.
pub fn object_editor(
    ui: &mut egui::Ui,
    state: &mut ObjectEditorState,
    editing: &ObjectEditing,
    ctx: &ObjectEditorContext,
) -> Option<ObjectEditorAction> {
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(Spacing::XL as i8, Spacing::S as i8))
        .show(ui, |ui| object_editor_body(ui, state, editing, ctx))
        .inner
}

fn object_editor_body(
    ui: &mut egui::Ui,
    state: &mut ObjectEditorState,
    editing: &ObjectEditing,
    ctx: &ObjectEditorContext,
) -> Option<ObjectEditorAction> {
    let mut action = None;
    let kind = state.kind();
    let pending = state.pending();
    let problems = state.problems(&editing.column_types);
    let blocking = problems.iter().any(|p| p.error);
    let p = theme::palette(ui.ctx());

    // Name line.
    ui.add_space(Spacing::S);
    ui.horizontal(|ui| {
        let (icon, color) = kind_icon(kind);
        icons::paint(ui, icon, color(&p));
        caption(ui, &kind.keyword().to_lowercase());
        let can_rename = state.is_new() || editing.rename.contains(&kind);
        let name = name_mut(&mut state.after);
        let response = ui
            .add_enabled(
                can_rename && ctx.busy.is_none(),
                TextEdit::singleline(name)
                    .desired_width(240.0)
                    .hint_text("name"),
            )
            .on_disabled_hover_text("This object cannot be renamed here");
        crate::text_menu::text_edit_menu(&response, crate::text_menu::TextMenuMode::Editable);
        let schema = state.after.object_ref().schema;
        if let Some(schema) = schema {
            caption(ui, &format!("in {schema}"));
        }
        if state.is_new() {
            ui.label(RichText::new("new").color(p.success));
        }
    });

    // Toolbar.
    ui.add_space(Spacing::S);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::S;
        let idle = ctx.busy.is_none();
        let can_apply = idle && pending > 0 && !blocking && ctx.apply_blocked.is_none();
        let preview = icons::button(ui, idle && pending > 0, icons::SEARCH, None, "Preview DDL");
        if preview.clicked() {
            action = Some(ObjectEditorAction::Preview);
        }
        let apply = icons::button(ui, can_apply, icons::SUCCESS, Some(p.success), "Apply")
            .on_hover_text(crate::keymap::tooltip(
                ui.ctx(),
                "Apply the pending changes",
                sqlr_core::ActionId::Apply,
            ));
        let apply = match ctx.apply_blocked {
            Some(reason) => apply.on_disabled_hover_text(reason),
            None => apply,
        };
        if apply.clicked() {
            action = Some(ObjectEditorAction::Apply);
        }
        let revert = icons::button(
            ui,
            idle && pending > 0 && !state.is_new(),
            icons::CANCELLED,
            None,
            "Revert",
        );
        if revert.clicked() {
            state.revert();
            action = Some(ObjectEditorAction::Revert);
        }
        let refresh = icons::button(ui, idle && !state.is_new(), icons::REFRESH, None, "Refresh")
            .on_hover_text(crate::keymap::tooltip(
                ui.ctx(),
                "Load the definition again",
                sqlr_core::ActionId::Refresh,
            ));
        if refresh.clicked() {
            action = Some(ObjectEditorAction::Refresh);
        }
        ui.separator();
        ui.add_enabled_ui(!state.is_new(), |ui| {
            ui.menu_button("Generate SQL", |ui| {
                for template in templates_for(kind) {
                    if ui.button(template_label(template)).clicked() {
                        action = Some(ObjectEditorAction::GenerateSql(template));
                        ui.close();
                    }
                }
            });
        });
        let opens_data = matches!(
            kind,
            ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView
        );
        if opens_data
            && icons::button(ui, !state.is_new(), icons::ROWS, None, "Open data").clicked()
        {
            action = Some(ObjectEditorAction::OpenData);
        }
        ui.separator();
        let text = match pending {
            0 => "No pending changes".to_string(),
            1 => "1 pending change".to_string(),
            n => format!("{n} pending changes"),
        };
        let color = if pending > 0 { p.warning } else { p.text_muted };
        ui.label(RichText::new(text).color(color));
        if let Some(busy) = ctx.busy {
            ui.spinner();
            caption(ui, busy);
        }
    });

    if let Some((text, error)) = ctx.message {
        let color = if error { p.error } else { p.success };
        ui.label(RichText::new(text).color(color));
    }
    for problem in problems.iter().take(3) {
        let color = if problem.error { p.error } else { p.warning };
        ui.horizontal(|ui| {
            icons::paint(ui, icons::WARNING, color);
            ui.label(RichText::new(&problem.text).color(color));
        });
    }
    if problems.len() > 3 {
        caption(ui, &format!("…and {} more", problems.len() - 3));
    }

    // Sub-tabs.
    let tabs = state.sub_tabs();
    if !tabs.contains(&state.tab) {
        state.tab = tabs[0];
    }
    ui.add_space(Spacing::S);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = Spacing::XS;
        for tab in &tabs {
            if theme::dialog_tab(ui, state.tab == *tab, tab.label()).clicked() {
                state.tab = *tab;
            }
        }
    });
    ui.separator();

    let editable = (state.is_new() || editing.alter.contains(&kind)) && ctx.busy.is_none();
    let tab = state.tab;
    match tab {
        ObjectSubTab::Ddl => pending::ddl_tab(ui, ctx),
        // The caller draws the page (the result tabs' grid, changes bar and editing).
        ObjectSubTab::Data => {
            if !std::mem::replace(&mut state.data_requested, true) {
                action = action.or(Some(ObjectEditorAction::LoadData));
            }
        }
        // The caller draws the privilege grid of the object's grants.
        ObjectSubTab::Permissions => {
            if !std::mem::replace(&mut state.permissions_requested, true) {
                action = action.or(Some(ObjectEditorAction::LoadPermissions));
            }
        }
        _ => {
            let response = ui.add_enabled_ui(editable, |ui| content(ui, state, tab, editing, ctx));
            if let Some(a) = response.inner {
                action = action.or(Some(a));
            }
            if tab == ObjectSubTab::Properties && state.statistics {
                if !std::mem::replace(&mut state.stats_requested, true) {
                    action = action.or(Some(ObjectEditorAction::LoadStats));
                }
                ui.add_space(Spacing::L);
                ui.label(RichText::new("Statistics").strong());
                match ctx.stats {
                    Some(stats) => crate::admin::table_stats_detail(ui, stats),
                    None => {
                        caption(ui, "Loading…");
                    }
                }
            }
        }
    }
    action
}

/// The form of an editable sub-tab.
fn content(
    ui: &mut egui::Ui,
    state: &mut ObjectEditorState,
    tab: ObjectSubTab,
    editing: &ObjectEditing,
    ctx: &ObjectEditorContext,
) -> Option<ObjectEditorAction> {
    let column_order = state.is_new() || editing.column_order;
    let ObjectEditorState {
        after,
        column,
        scroll_to_column,
        index,
        constraint,
        foreign_key,
        trigger,
        attach,
        ..
    } = state;
    match (tab, after) {
        (ObjectSubTab::Columns, ObjectDef::Table(t)) => columns::columns_tab(
            ui,
            t,
            column,
            scroll_to_column,
            editing,
            column_order,
            &ctx.syntax,
        ),
        (ObjectSubTab::Indexes, ObjectDef::Table(t)) => {
            let columns: Vec<String> = t.columns.iter().map(|c| c.name.clone()).collect();
            indexes::indexes_tab(ui, &mut t.indexes, index, &columns, &ctx.syntax)
        }
        (ObjectSubTab::Constraints, ObjectDef::Table(t)) => {
            constraints::constraints_tab(ui, t, constraint, &ctx.syntax)
        }
        (ObjectSubTab::ForeignKeys, ObjectDef::Table(t)) => {
            return foreign_keys::foreign_keys_tab(ui, t, foreign_key, ctx);
        }
        (ObjectSubTab::Triggers, ObjectDef::Table(t)) => {
            let table = t.table.clone();
            triggers::triggers_tab(ui, &mut t.triggers, &table, trigger, &ctx.syntax)
        }
        (ObjectSubTab::Partitions, ObjectDef::Table(t)) => {
            properties::partitions_tab(ui, t, attach, ctx)
        }
        (ObjectSubTab::Properties, def) => properties::properties_tab(ui, def, editing),
        (ObjectSubTab::Definition, ObjectDef::View(v)) => view::view_tab(ui, v, editing, ctx),
        (ObjectSubTab::Definition, ObjectDef::Routine(r)) => routine::routine_tab(ui, r, ctx),
        (ObjectSubTab::Definition, ObjectDef::Sequence(s)) => {
            properties::sequence_tab(ui, s, editing)
        }
        (ObjectSubTab::Definition, ObjectDef::Index { table, index }) => {
            indexes::index_object_tab(ui, table, index, ctx)
        }
        (ObjectSubTab::Definition, ObjectDef::Trigger(t)) => {
            triggers::trigger_object_tab(ui, t, ctx)
        }
        _ => {
            caption(ui, "Nothing to edit here");
        }
    }
    None
}
