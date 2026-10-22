//! The object tab: an object editor in the dock, its load and apply jobs, its DDL and its
//! Data page. It lives inside a dock tab (`QueryTab::object`); the app handles the actions
//! the editor returns once the dock is drawn.

use std::sync::Arc;

use egui::RichText;
use sqlr_core::{
    AdminSupport, CatalogNeed, Dialect, Grant, ObjectDef, ObjectEditing, ObjectKind, ObjectRef,
    RelationKind, RoleDef, SchemaChange, SqlSyntax, SqlTemplate, TableRef, TableStats,
};
use sqlr_engine::{AdminQuery, AdminSnapshot, ApplyOptions, Engine, JobId, JobOutcome, SessionKey};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{
    ObjectEditorAction, ObjectEditorContext, ObjectEditorState, PrivilegeGridState, PrivilegeRow,
    object_editor, privilege_grid,
};

/// Catalog tables with their columns: foreign key and index pickers.
type Tables = Arc<Vec<(TableRef, Vec<String>)>>;

/// The pending statements of a `(before, after)` pair, or why there are none.
type PendingDdl = (Option<ObjectDef>, ObjectDef, Result<Vec<String>, String>);

/// An object editor tab.
pub struct ObjectTab {
    pub conn: String,
    /// The object as last loaded (or to load).
    pub object: ObjectRef,
    /// Explorer paths to reload once an apply changed something.
    pub refresh: Vec<sqlr_core::NodePath>,
    /// `None` until the first load arrives (or for a load that failed).
    pub state: Option<ObjectEditorState>,
    pub editing: ObjectEditing,
    pub load: Option<JobId>,
    pub apply: Option<JobId>,
    pub load_error: Option<String>,
    /// The running load only refreshes `before`: the edits stay (after a partial,
    /// non-transactional apply, so only what did not apply stays pending).
    keep_edits: bool,
    /// The last apply's outcome: `(text, is_error)`.
    pub message: Option<(String, bool)>,
    /// The Data sub-tab's page (the hosting tab's run) shows a definition that changed: the
    /// host closes it, and the editor asks for it again.
    data_stale: bool,
    current_ddl: Option<(ObjectDef, Result<String, String>)>,
    /// The pending statements of one `(before, after)` pair.
    pending_ddl: Option<PendingDdl>,
    /// The catalog revision they were built from.
    tables: Option<(u64, Tables)>,
    /// Grants and statistics.
    admin: ObjectAdmin,
}

/// An object tab's administration: its grants per role (Permissions) and its statistics.
#[derive(Default)]
struct ObjectAdmin {
    /// What the driver administers, once connected.
    support: Option<AdminSupport>,
    grid: PrivilegeGridState,
    roles: Vec<RoleDef>,
    roles_job: Option<JobId>,
    grants_job: Option<JobId>,
    stats_job: Option<JobId>,
    apply: Option<JobId>,
    stats: Option<TableStats>,
    /// `(text, is_error)`.
    message: Option<(String, bool)>,
    /// "Apply grants" was clicked since the last call.
    apply_requested: bool,
}

impl ObjectTab {
    /// A tab that loads `object`.
    pub fn open(
        engine: &Engine,
        conn: &str,
        object: ObjectRef,
        editing: ObjectEditing,
        refresh: Vec<sqlr_core::NodePath>,
    ) -> Self {
        let load = Some(engine.load_object(conn, object.clone()));
        Self::with(conn, object, editing, refresh, None, load)
    }

    /// A tab that creates `def`.
    pub fn create(
        conn: &str,
        def: ObjectDef,
        database: Option<String>,
        editing: ObjectEditing,
        refresh: Vec<sqlr_core::NodePath>,
    ) -> Self {
        let object = ObjectRef {
            database,
            ..def.object_ref()
        };
        let state = Some(ObjectEditorState::new_object(def));
        Self::with(conn, object, editing, refresh, state, None)
    }

    fn with(
        conn: &str,
        object: ObjectRef,
        editing: ObjectEditing,
        refresh: Vec<sqlr_core::NodePath>,
        state: Option<ObjectEditorState>,
        load: Option<JobId>,
    ) -> Self {
        Self {
            conn: conn.to_string(),
            object,
            refresh,
            state,
            editing,
            load,
            apply: None,
            load_error: None,
            keep_edits: false,
            message: None,
            data_stale: false,
            current_ddl: None,
            pending_ddl: None,
            tables: None,
            admin: ObjectAdmin::default(),
        }
    }

    /// The tab title: the object's name.
    pub fn title(&self) -> String {
        match &self.state {
            Some(s) => s.name(),
            None => self.object.name.clone(),
        }
    }

    pub fn pending(&mut self) -> usize {
        // The Permissions grid's edits count too: a refresh or a close would drop them.
        self.state.as_mut().map_or(0, |s| s.pending()) + self.admin.grid.pending()
    }

    /// The server database the object lives in, when it is not the connection's own.
    pub fn database(&self) -> Option<String> {
        self.object.database.clone()
    }

    /// Forgets the tab's jobs (its data page is the hosting tab's run).
    pub fn close(&mut self, engine: &Engine) {
        let a = &mut self.admin;
        let jobs = [
            self.load.take(),
            self.apply.take(),
            a.roles_job.take(),
            a.grants_job.take(),
            a.stats_job.take(),
            a.apply.take(),
        ];
        for id in jobs.into_iter().flatten() {
            engine.forget_job(id);
        }
    }

    /// The object the Permissions grid edits grants on: as loaded, in the session's
    /// database (Permissions are offered only there).
    fn grant_object(&self) -> Option<ObjectRef> {
        let before = self.state.as_ref()?.before.as_ref()?;
        Some(ObjectRef {
            database: None,
            ..before.object_ref()
        })
    }

    /// Loads the roles and the object's grants (the Permissions sub-tab).
    pub fn load_permissions(&mut self, engine: &Engine) {
        let Some(object) = self.grant_object() else {
            return;
        };
        let a = &mut self.admin;
        if a.support.as_ref().is_some_and(|s| s.roles) {
            a.roles_job = Some(engine.admin_snapshot(&self.conn, AdminQuery::Roles));
        }
        let query = AdminQuery::Grants {
            grantee: None,
            object: Some(object),
        };
        a.grants_job = Some(engine.admin_snapshot(&self.conn, query));
    }

    /// Loads the statistics of the object's schema (Properties).
    pub fn load_stats(&mut self, engine: &Engine) {
        let schema = self.object.schema.clone();
        let query = AdminQuery::TableStats { schema };
        self.admin.stats_job = Some(engine.admin_snapshot(&self.conn, query));
    }

    /// Whether "Apply grants" was clicked since the last call.
    pub fn take_apply_grants(&mut self) -> bool {
        std::mem::take(&mut self.admin.apply_requested)
    }

    /// The statements that apply the grid's changes, and whether any revokes.
    pub fn grant_statements(&self, engine: &Engine) -> Result<Vec<String>, String> {
        let (add, revoke) = self.admin.grid.changes();
        engine
            .grant_ddl(&self.conn, &add, &revoke)
            .map_err(|e| e.message)
    }

    /// Starts applying grant statements (after the confirmations).
    pub fn start_grant_apply(
        &mut self,
        engine: &Engine,
        statements: Vec<String>,
        options: ApplyOptions,
    ) {
        self.admin.message = None;
        self.admin.apply =
            Some(engine.apply_ddl(&SessionKey::meta(&self.conn), statements, options));
    }

    fn poll_admin(&mut self, engine: &Engine) {
        let object = self.grant_object();
        let a = &mut self.admin;
        if let Some(outcome) = crate::admin::take(engine, &mut a.roles_job) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Roles(roles)) => a.roles = roles,
                JobOutcome::Failed(e) => a.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = crate::admin::take(engine, &mut a.grants_job) {
            match (outcome, object) {
                (JobOutcome::Admin(AdminSnapshot::Grants(grants)), Some(object)) => {
                    // Every grant is on this object: one form for the grid's keys.
                    let grants: Vec<Grant> = grants
                        .into_iter()
                        .map(|g| Grant {
                            object: object.clone(),
                            ..g
                        })
                        .collect();
                    a.grid.set_grants(&grants);
                }
                (JobOutcome::Failed(e), _) => a.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = crate::admin::take(engine, &mut a.stats_job) {
            let name = &self.object.name;
            match outcome {
                JobOutcome::Admin(AdminSnapshot::TableStats(stats)) => {
                    a.stats = stats.into_iter().find(|s| s.table.name == *name);
                }
                JobOutcome::Failed(e) => a.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = crate::admin::take(engine, &mut a.apply) {
            match outcome {
                JobOutcome::DdlApplied(report) => {
                    a.message = Some((report.summary(), !report.ok()));
                    if report.applied() > 0 {
                        self.load_permissions(engine);
                    }
                }
                JobOutcome::Failed(e) => a.message = Some((e.message, true)),
                JobOutcome::Cancelled => a.message = Some(("Cancelled".into(), true)),
                _ => {}
            }
        }
    }

    /// The Permissions sub-tab: the object's grants per role, with Apply and Revert.
    fn permissions_ui(&mut self, ui: &mut egui::Ui) {
        let Some(object) = self.grant_object() else {
            return;
        };
        let a = &mut self.admin;
        let Some(support) = a.support.clone() else {
            return;
        };
        let p = theme::palette(ui.ctx());
        let pending = a.grid.pending();
        let busy = a.apply.is_some();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = Spacing::S;
            if icons::button(
                ui,
                pending > 0 && !busy,
                icons::SUCCESS,
                Some(p.success),
                "Apply grants",
            )
            .clicked()
            {
                a.apply_requested = true;
            }
            if icons::button(
                ui,
                pending > 0 && !busy,
                icons::CANCELLED,
                None,
                "Revert grants",
            )
            .clicked()
            {
                a.grid.revert();
            }
            if busy || a.grants_job.is_some() {
                ui.spinner();
            }
        });
        if let Some((text, error)) = &a.message {
            let color = if *error { p.error } else { p.success };
            ui.label(RichText::new(text).color(color));
        }
        let mut rows: Vec<PrivilegeRow> = a
            .roles
            .iter()
            .map(|r| PrivilegeRow {
                grantee: r.grantee(),
                object: object.clone(),
                label: r.grantee(),
            })
            .collect();
        // Grantees the role list does not hold (PUBLIC, roles not readable).
        for g in a.grid.grantees() {
            if !rows.iter().any(|r| r.grantee == g) {
                rows.push(PrivilegeRow {
                    grantee: g.clone(),
                    object: object.clone(),
                    label: g,
                });
            }
        }
        ui.add_enabled_ui(!busy, |ui| {
            privilege_grid(ui, &mut a.grid, &rows, &support);
        });
    }

    /// Whether the hosting tab must drop its run (the Data page) since the last call.
    pub fn take_data_stale(&mut self) -> bool {
        std::mem::take(&mut self.data_stale)
    }

    /// The Data sub-tab is shown: the hosting tab draws its run below the editor.
    pub fn data_shown(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.data_shown())
    }

    /// The statements that apply the pending changes: a table through `table_ddl` (a SQLite
    /// rebuild needs both definitions), a new object through its creation.
    pub fn statements(&self, engine: &Engine) -> Result<Vec<String>, String> {
        let state = self.state.as_ref().ok_or("Nothing loaded")?;
        let result = match (&state.before, &state.after) {
            (None, after) => engine.preview_ddl(&self.conn, &[SchemaChange::Create(after.clone())]),
            (Some(ObjectDef::Table(b)), ObjectDef::Table(a)) => engine.table_ddl(&self.conn, b, a),
            (Some(before), after) => {
                engine.preview_ddl(&self.conn, &sqlr_core::diff_object(before, after))
            }
        };
        result.map_err(|e| e.message)
    }

    /// How this tab's applies run: the driver's transactional DDL, the object's database,
    /// and the explorer paths to reload.
    pub fn apply_options(&self) -> ApplyOptions {
        ApplyOptions {
            transactional: self.editing.transactional_ddl,
            database: self.database(),
            refresh: self.refresh.clone(),
            masking: None,
        }
    }

    /// Starts applying `statements` with `options` (from [`ObjectTab::apply_options`]).
    pub fn start_apply(&mut self, engine: &Engine, statements: Vec<String>, options: ApplyOptions) {
        self.message = None;
        self.apply = Some(engine.apply_ddl(&SessionKey::meta(&self.conn), statements, options));
    }

    /// Loads the definition again, dropping the edits (Refresh).
    pub fn reload(&mut self, engine: &Engine) {
        if self.state.as_ref().is_some_and(|s| s.is_new()) {
            return;
        }
        self.object = ObjectRef {
            database: self.object.database.clone(),
            ..self.loaded_ref()
        };
        self.keep_edits = false;
        self.message = None;
        self.load = Some(engine.load_object(&self.conn, self.object.clone()));
        self.data_stale = true;
    }

    /// The reference of the definition as loaded (`before`), else the tab's.
    fn loaded_ref(&self) -> ObjectRef {
        self.state
            .as_ref()
            .and_then(|s| s.before.as_ref())
            .map_or_else(|| self.object.clone(), |b| b.object_ref())
    }

    /// Follows the tab's jobs; returns a notice for the status bar when an apply ended.
    pub fn poll(&mut self, engine: &Engine) {
        if let Some(id) = self.apply
            && let Some(job) = engine.job(id).and_then(|j| j.outcome)
        {
            engine.forget_job(id);
            self.apply = None;
            match job {
                JobOutcome::DdlApplied(report) if report.ok() => {
                    self.message = Some((report.summary(), false));
                    // The applied definition names the object now.
                    if let Some(state) = &self.state {
                        self.object = ObjectRef {
                            database: self.object.database.clone(),
                            ..state.after.object_ref()
                        };
                    }
                    self.load = Some(engine.load_object(&self.conn, self.object.clone()));
                    self.data_stale = true;
                }
                JobOutcome::DdlApplied(report) => {
                    self.message = Some((report.summary(), true));
                    if keeps_edits(&report) {
                        // What applied is no longer pending: load the object as it is now
                        // and keep the edits on top of it.
                        self.keep_edits = true;
                        self.object = ObjectRef {
                            database: self.object.database.clone(),
                            ..self.loaded_ref()
                        };
                        self.load = Some(engine.load_object(&self.conn, self.object.clone()));
                    }
                }
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                JobOutcome::Cancelled => self.message = Some(("Cancelled".into(), true)),
                _ => {}
            }
        }
        if let Some(id) = self.load
            && let Some(job) = engine.job(id).and_then(|j| j.outcome)
        {
            engine.forget_job(id);
            self.load = None;
            match job {
                JobOutcome::Object(def) => {
                    self.load_error = None;
                    match &mut self.state {
                        Some(state) if std::mem::take(&mut self.keep_edits) => {
                            state.before = Some(def)
                        }
                        Some(state) => state.reload(def),
                        None => self.state = Some(ObjectEditorState::new(def)),
                    }
                }
                // With a definition on screen, the failure joins the message line.
                JobOutcome::Failed(e) if self.state.is_some() => {
                    self.keep_edits = false;
                    let failed = format!("Reloading the definition failed: {}", e.message);
                    self.message = Some(match self.message.take() {
                        Some((text, true)) => (format!("{text} {failed}"), true),
                        _ => (failed, true),
                    });
                }
                JobOutcome::Failed(e) => self.load_error = Some(e.message),
                _ => {}
            }
        }
    }

    fn refresh_ddl(&mut self, engine: &Engine) {
        let Some(state) = &self.state else {
            return;
        };
        match &state.before {
            None => self.current_ddl = None,
            Some(before) => {
                if self.current_ddl.as_ref().is_none_or(|(d, _)| d != before) {
                    let ddl = engine.create_ddl(&self.conn, before).map_err(|e| e.message);
                    self.current_ddl = Some((before.clone(), ddl));
                }
            }
        }
        if self
            .pending_ddl
            .as_ref()
            .is_none_or(|(b, a, _)| *a != state.after || *b != state.before)
        {
            let (before, after) = (state.before.clone(), state.after.clone());
            let statements = self.statements(engine);
            self.pending_ddl = Some((before, after, statements));
        }
    }

    /// The catalog's tables with their columns (cached per catalog revision).
    fn tables(&mut self, engine: &Engine) -> Tables {
        let revision = engine.catalog_revision();
        if let Some((r, tables)) = &self.tables
            && *r == revision
        {
            return tables.clone();
        }
        let mut out = Vec::new();
        if let Some(catalog) = engine.catalog(&self.conn) {
            for (schema, state) in &catalog.schema {
                if let sqlr_engine::LoadState::Ready(c) = state {
                    for r in c.relations.iter().filter(|r| r.kind == RelationKind::Table) {
                        out.push((
                            TableRef {
                                database: None,
                                schema: Some(schema.clone()),
                                name: r.name.clone(),
                            },
                            r.columns.iter().map(|c| c.name.clone()).collect(),
                        ));
                    }
                }
            }
        }
        let tables = Arc::new(out);
        self.tables = Some((revision, tables.clone()));
        tables
    }

    /// Asks for the catalog of the schema the object is in (foreign key pickers).
    pub fn load_tables(&self, engine: &Engine, dialect: Option<&Arc<dyn Dialect>>) {
        let schemas = match &self.object.schema {
            Some(s) => vec![s.clone()],
            None => dialect.map(|d| d.default_schemas()).unwrap_or_default(),
        };
        for schema in schemas {
            engine.load_catalog(&self.conn, CatalogNeed::Schema(schema));
        }
    }

    /// The SQL that reads the object's first page.
    pub fn select_sql(&self, engine: &Engine) -> Option<String> {
        let before = self.state.as_ref()?.before.as_ref()?;
        let sql = engine
            .generate_sql(&self.conn, SqlTemplate::Select, before)
            .ok()?;
        Some(sqlr_core::trim_statement(&sql).to_string())
    }

    /// Draws the editor; returns its action. The Data sub-tab's page is drawn by the
    /// hosting tab below (see [`ObjectTab::data_shown`]).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        syntax: Arc<SqlSyntax>,
    ) -> Option<ObjectEditorAction> {
        self.poll(engine);
        self.poll_admin(engine);
        if self.admin.support.is_none() {
            self.admin.support = engine.admin_support(&self.conn);
        }
        // Grants and statistics are read in the session's own database only.
        let own_database = self.object.database.is_none();
        if let (Some(state), Some(support)) = (&mut self.state, &self.admin.support) {
            let kind = state.kind();
            state.permissions =
                own_database && support.grants && !support.privileges_of(kind).is_empty();
            state.statistics = own_database
                && support.statistics
                && matches!(kind, ObjectKind::Table | ObjectKind::MaterializedView);
        }
        if self.state.is_none() {
            ui.add_space(Spacing::M);
            match &self.load_error {
                Some(e) => {
                    ui.colored_label(theme::palette(ui.ctx()).error, e);
                    if ui.button("Retry").clicked() {
                        return Some(ObjectEditorAction::Refresh);
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Loading…"));
                    });
                }
            }
            return None;
        }
        self.refresh_ddl(engine);
        let tables = self.tables(engine);
        // As the engine's apply decides it.
        let blocked = crate::admin::actions_blocked(engine, &self.conn, true);
        let busy = if self.apply.is_some() {
            Some("Applying…")
        } else if self.load.is_some() {
            Some("Loading…")
        } else {
            None
        };
        let current = self.current_ddl.as_ref().map(|(_, d)| match d {
            Ok(ddl) => ddl.as_str(),
            Err(e) => e.as_str(),
        });
        let pending = self.pending_ddl.as_ref().and_then(|(_, _, p)| match p {
            Ok(statements) if statements.is_empty() => None,
            Ok(statements) => Some(Ok(statements.as_slice())),
            Err(e) => Some(Err(e.as_str())),
        });
        let message = self.message.as_ref().map(|(t, e)| (t.as_str(), *e));
        let ctx = ObjectEditorContext {
            tables: &tables,
            syntax,
            current_ddl: current,
            pending_ddl: pending,
            busy,
            apply_blocked: blocked,
            message,
            stats: self.admin.stats.as_ref(),
        };
        let state = self.state.as_mut()?;
        let action = object_editor(ui, state, &self.editing, &ctx);
        let permissions = state.permissions_shown();
        if permissions {
            self.permissions_ui(ui);
        }
        action
    }
}

/// After a failed apply, whether the object must be loaded again under the edits: anything
/// that stayed applied (a non-transactional statement, or one run alone) is no longer
/// pending.
fn keeps_edits(report: &sqlr_engine::DdlReport) -> bool {
    !report.ok() && report.applied() > 0
}

#[cfg(test)]
mod tests {
    use sqlr_core::{DriverError, ErrorKind};
    use sqlr_engine::{DdlReport, DdlStepOutcome};

    use super::keeps_edits;

    #[test]
    fn edits_stay_on_whatever_applied() {
        let e = DriverError::new(ErrorKind::Syntax, "bad");
        let report = |outcomes: Vec<DdlStepOutcome>, transactional| DdlReport {
            statements: outcomes.into_iter().map(|o| (String::new(), o)).collect(),
            rolled_back: transactional,
            transactional,
            control: Vec::new(),
        };
        // An alone statement applied before the failed transaction.
        let alone = report(
            vec![
                DdlStepOutcome::Applied,
                DdlStepOutcome::RolledBack,
                DdlStepOutcome::Failed(e.clone()),
            ],
            true,
        );
        assert!(keeps_edits(&alone));
        let none = report(
            vec![DdlStepOutcome::RolledBack, DdlStepOutcome::Failed(e)],
            true,
        );
        assert!(!keeps_edits(&none));
        assert!(!keeps_edits(&report(vec![DdlStepOutcome::Applied], false)));
    }
}
