//! The "Users and roles" tab: the role list, the role form and the selected role's
//! privilege grid. Form and grid changes are one pending set: previewed as the dialect's
//! statements (passwords masked) and applied together as DDL through the engine's apply.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{Id, RichText};
use sqlr_core::{
    AdminSupport, Dialect, Grant, ObjectKind, ObjectRef, RelationKind, RoleDef, SqlSyntax, script,
};
use sqlr_engine::{
    AdminQuery, AdminSnapshot, ApplyOptions, Engine, JobId, JobOutcome, LoadState, Masking,
    SessionKey,
};
use sqlr_ui::theme::{self, Spacing, icons};
use sqlr_ui::{
    PrivilegeGridState, PrivilegeRow, RoleEditorState, privilege_grid, role_form, role_list,
    sql_view,
};

use super::{AdminCommand, take};

/// The grid's grantee while the role is new (it has no name yet).
const NEW_ROLE: &str = "";

/// What the users tab does that drops its pending edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsersDiscard {
    /// Select another role (by grantee name).
    Select(String),
    NewRole,
    /// Load the roles again.
    Reload,
}

/// Why the pending role changes cannot be turned into statements.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RoleChangesError {
    /// The role editor has a problem the person must fix first.
    #[error("{0}")]
    Problem(String),
    /// The driver could not build the statements.
    #[error(transparent)]
    Driver(#[from] sqlr_core::DriverError),
}

/// What applying the users tab runs.
pub(crate) struct RoleChanges {
    /// The statements as run (with the password).
    pub statements: Vec<String>,
    /// How the job runs and reports them (masked).
    pub options: ApplyOptions,
}

pub struct UsersTab {
    pub conn: String,
    support: AdminSupport,
    current_user: Option<String>,
    roles: Vec<RoleDef>,
    roles_job: Option<JobId>,
    grants_job: Option<JobId>,
    /// The selected role's grants as loaded.
    loaded: Vec<Grant>,
    filter: String,
    /// The selected role's grantee name.
    selected: Option<String>,
    /// Select this role once the roles are loaded again (after an apply).
    reselect: Option<String>,
    editor: Option<RoleEditorState>,
    grid: PrivilegeGridState,
    /// The grid's schema.
    schema: Option<String>,
    pub apply: Option<JobId>,
    /// `(text, is_error)`.
    message: Option<(String, bool)>,
    preview: bool,
    /// Bumps whenever `loaded` changes.
    loaded_generation: u64,
    /// The grid's rows and what they were built from.
    rows: Option<(RowsKey, Arc<Vec<PrivilegeRow>>)>,
    /// How many times the rows were built (tests).
    pub(crate) rows_built: usize,
}

/// What the grid's rows depend on: the catalog revision, the schema, the loaded grants'
/// generation, the grid's grantee and the connection's database.
type RowsKey = (u64, Option<String>, u64, String, Option<String>);

impl UsersTab {
    pub fn new(engine: &Engine, conn: &str) -> Self {
        let mut grid = PrivilegeGridState::default();
        grid.hide_pending = true;
        Self {
            conn: conn.to_string(),
            support: engine.admin_support(conn).unwrap_or_default(),
            current_user: engine.admin_current_user(conn),
            roles: Vec::new(),
            roles_job: Some(engine.admin_snapshot(conn, AdminQuery::Roles)),
            grants_job: None,
            loaded: Vec::new(),
            filter: String::new(),
            selected: None,
            reselect: None,
            editor: None,
            grid,
            schema: None,
            apply: None,
            message: None,
            preview: false,
            loaded_generation: 0,
            rows: None,
            rows_built: 0,
        }
    }

    pub fn jobs(&self) -> Vec<JobId> {
        [self.roles_job, self.grants_job, self.apply]
            .into_iter()
            .flatten()
            .collect()
    }

    /// The pending changes: the form's and the grid's.
    pub fn pending(&self) -> usize {
        usize::from(self.editor.as_ref().is_some_and(|e| e.pending())) + self.grid.pending()
    }

    /// Whether Apply and Revert act now: edits are pending and no apply runs.
    pub fn editable_now(&self) -> bool {
        self.pending() > 0 && self.apply.is_none()
    }

    /// Drops the role's and the grid's edits, as Revert does.
    pub fn revert(&mut self) {
        if !self.editable_now() {
            return;
        }
        if let Some(editor) = &mut self.editor {
            editor.revert();
        }
        self.grid.revert();
    }

    fn set_loaded(&mut self, grants: Vec<Grant>) {
        self.grid.set_grants(&grants);
        self.loaded = grants;
        self.loaded_generation += 1;
    }

    fn select(&mut self, engine: &Engine, grantee: &str) {
        let Some(role) = self.roles.iter().find(|r| r.grantee() == grantee) else {
            return;
        };
        self.selected = Some(grantee.to_string());
        self.editor = Some(RoleEditorState::new(role.clone()));
        self.set_loaded(Vec::new());
        self.preview = false;
        if self.support.grants {
            let query = AdminQuery::Grants {
                grantee: Some(grantee.to_string()),
                object: None,
            };
            let job = engine.admin_snapshot(&self.conn, query);
            if let Some(old) = self.grants_job.replace(job) {
                engine.forget_job(old);
            }
        }
    }

    fn new_role(&mut self) {
        self.selected = None;
        self.editor = Some(RoleEditorState::new_role(self.support.hosts));
        self.set_loaded(Vec::new());
        self.preview = false;
        self.message = None;
    }

    /// Does what drops the pending edits (asked first when there are any).
    pub fn perform(&mut self, engine: &Engine, action: UsersDiscard) {
        match action {
            UsersDiscard::Select(grantee) => self.select(engine, &grantee),
            UsersDiscard::NewRole => self.new_role(),
            UsersDiscard::Reload => {
                self.reselect = self.selected.clone();
                self.editor = None;
                self.selected = None;
                self.set_loaded(Vec::new());
                self.roles_job = Some(engine.admin_snapshot(&self.conn, AdminQuery::Roles));
            }
        }
    }

    /// What the discard question calls the edited role.
    pub fn edited_name(&self) -> String {
        match self.editor.as_ref().and_then(|e| e.before.as_ref()) {
            Some(before) => before.grantee(),
            None => "the new role".to_string(),
        }
    }

    /// The grid's grantee: the role as loaded, or [`NEW_ROLE`].
    fn grid_grantee(&self) -> String {
        self.editor
            .as_ref()
            .and_then(|e| e.before.as_ref())
            .map_or_else(|| NEW_ROLE.to_string(), |b| b.grantee())
    }

    fn poll(&mut self, engine: &Engine) {
        if let Some(outcome) = take(engine, &mut self.roles_job) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Roles(roles)) => {
                    self.roles = roles;
                    if let Some(grantee) = self.reselect.take() {
                        self.select(engine, &grantee);
                    }
                }
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = take(engine, &mut self.grants_job) {
            match outcome {
                JobOutcome::Admin(AdminSnapshot::Grants(grants)) => self.set_loaded(grants),
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                _ => {}
            }
        }
        if let Some(outcome) = take(engine, &mut self.apply) {
            match outcome {
                JobOutcome::DdlApplied(report) if report.ok() => {
                    self.message = Some((report.summary(), false));
                    self.reselect = self.editor.as_ref().map(|e| e.after.grantee());
                    self.editor = None;
                    self.selected = None;
                    self.set_loaded(Vec::new());
                    self.preview = false;
                    self.roles_job = Some(engine.admin_snapshot(&self.conn, AdminQuery::Roles));
                }
                JobOutcome::DdlApplied(report) => self.message = Some((report.summary(), true)),
                JobOutcome::Failed(e) => self.message = Some((e.message, true)),
                JobOutcome::Cancelled => self.message = Some(("Cancelled".into(), true)),
                _ => {}
            }
        }
    }

    /// The statements that apply the pending changes, as run and as shown; `None` when
    /// nothing is pending.
    pub(crate) fn changes(
        &self,
        engine: &Engine,
        transactional: bool,
    ) -> Result<Option<RoleChanges>, RoleChangesError> {
        let Some(editor) = &self.editor else {
            return Ok(None);
        };
        if let Some(problem) = editor.problems().into_iter().next() {
            return Err(RoleChangesError::Problem(problem));
        }
        let conn = &self.conn;
        let mut statements = Vec::new();
        let mut shown = Vec::new();
        if editor.pending() {
            let before = editor.before.as_ref();
            statements = engine.role_ddl(conn, before, &editor.after)?;
            shown = engine.role_ddl(conn, before, &editor.after.masked())?;
        }
        // The grid's grants go to the role's name as edited (a rename comes first).
        let grantee = editor.after.grantee();
        let (add, revoke) = self.grid.changes();
        let to_role = |grants: Vec<Grant>| -> Vec<Grant> {
            grants
                .into_iter()
                .map(|g| Grant {
                    grantee: grantee.clone(),
                    ..g
                })
                .collect()
        };
        let (add, revoke) = (to_role(add), to_role(revoke));
        let grants = engine.grant_ddl(conn, &add, &revoke)?;
        statements.extend(grants.iter().cloned());
        shown.extend(grants);
        if statements.is_empty() {
            return Ok(None);
        }
        let secrets = editor.after.password.iter().cloned().collect();
        Ok(Some(RoleChanges {
            statements,
            options: ApplyOptions {
                transactional,
                database: None,
                // No driver lists roles in the explorer, so no explorer path shows them; the
                // apply invalidates the catalog and the tab reloads its roles and grants.
                refresh: Vec::new(),
                masking: Some(Masking { shown, secrets }),
            },
        }))
    }

    /// Starts applying `statements` (after the confirmations).
    pub fn start_apply(&mut self, engine: &Engine, statements: Vec<String>, options: ApplyOptions) {
        self.message = None;
        self.apply = Some(engine.apply_ddl(&SessionKey::meta(&self.conn), statements, options));
    }

    /// The grid's rows, built again only when what they come from changed.
    pub(crate) fn privilege_rows(&mut self, engine: &Engine) -> Arc<Vec<PrivilegeRow>> {
        let database = engine.session_database(&self.conn);
        let key: RowsKey = (
            engine.catalog_revision(),
            self.schema.clone(),
            self.loaded_generation,
            self.grid_grantee(),
            database.clone(),
        );
        if let Some((built, rows)) = &self.rows
            && *built == key
        {
            return rows.clone();
        }
        let rows = Arc::new(self.build_rows(engine, database));
        self.rows_built += 1;
        self.rows = Some((key, rows.clone()));
        rows
    }

    /// The grid's rows: the database (PostgreSQL) and the schema itself, the schema's
    /// catalog relations, then other objects the role holds privileges on.
    fn build_rows(&self, engine: &Engine, database: Option<String>) -> Vec<PrivilegeRow> {
        let grantee = self.grid_grantee();
        let row = |object: ObjectRef, label: String| PrivilegeRow {
            grantee: grantee.clone(),
            object,
            label,
        };
        let named = |kind, schema: Option<&str>, name: &str| ObjectRef {
            kind,
            database: None,
            schema: schema.map(String::from),
            name: name.to_string(),
            parent: None,
            signature: None,
        };
        let has = |kind| !self.support.privileges_of(kind).is_empty();
        let mut rows = Vec::new();
        if has(ObjectKind::Database)
            && has(ObjectKind::Schema)
            && let Some(db) = database
        {
            let label = format!("database {db}");
            rows.push(row(named(ObjectKind::Database, None, &db), label));
        }
        if let Some(schema) = &self.schema {
            if has(ObjectKind::Schema) {
                let label = format!("schema {schema}");
                rows.push(row(named(ObjectKind::Schema, None, schema), label));
            } else if has(ObjectKind::Database) {
                // MySQL: a schema is a database.
                let label = format!("database {schema}");
                rows.push(row(named(ObjectKind::Database, None, schema), label));
            }
            if let Some(catalog) = engine.catalog(&self.conn)
                && let Some(LoadState::Ready(c)) = catalog.schema.get(schema)
            {
                for r in &c.relations {
                    let kind = match r.kind {
                        RelationKind::Table => ObjectKind::Table,
                        RelationKind::View => ObjectKind::View,
                        RelationKind::MaterializedView => ObjectKind::MaterializedView,
                    };
                    if has(kind) {
                        let label = format!("{schema}.{}", r.name);
                        rows.push(row(named(kind, Some(schema), &r.name), label));
                    }
                }
            }
        }
        let mut seen: HashSet<ObjectRef> = rows.iter().map(|r| r.object.clone()).collect();
        for g in &self.loaded {
            let in_view = match (&g.object.schema, &self.schema) {
                (Some(s), Some(wanted)) => s == wanted,
                _ => true,
            };
            if in_view && seen.insert(g.object.clone()) {
                let o = &g.object;
                let label = match (&o.schema, &o.signature) {
                    (Some(s), Some(sig)) => format!("{s}.{}({sig})", o.name),
                    (Some(s), None) => format!("{s}.{}", o.name),
                    (None, _) => {
                        let kind = sqlr_ui::kind_label(o.kind).to_lowercase();
                        format!("{kind} {}", o.name)
                    }
                };
                rows.push(row(o.clone(), label));
            }
        }
        rows
    }

    /// The schemas the grid can show: the catalog's and the dialect's defaults.
    fn schemas(&self, engine: &Engine, dialect: Option<&dyn Dialect>) -> Vec<String> {
        let mut out: Vec<String> = dialect.map(|d| d.default_schemas()).unwrap_or_default();
        if let Some(catalog) = engine.catalog(&self.conn) {
            for s in catalog.schema.keys() {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
        }
        for g in &self.loaded {
            if let Some(s) = &g.object.schema
                && !out.contains(s)
            {
                out.push(s.clone());
            }
        }
        out
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        engine: &Engine,
        dialect: Option<&dyn Dialect>,
        key: u64,
    ) -> Option<AdminCommand> {
        self.poll(engine);
        let mut command = None;
        if self.schema.is_none() {
            self.schema = self.schemas(engine, dialect).into_iter().next();
        }
        if let Some(schema) = &self.schema {
            engine.load_catalog(&self.conn, sqlr_core::CatalogNeed::Schema(schema.clone()));
        }
        let p = theme::palette(ui.ctx());
        egui::Panel::left(Id::new(("users_roles", key)))
            .resizable(true)
            .default_size(240.0)
            .min_size(160.0)
            .show(ui, |ui| {
                ui.add_space(Spacing::S);
                let mut wanted = None;
                ui.horizontal(|ui| {
                    if icons::button(
                        ui,
                        self.support.roles,
                        icons::ADD,
                        Some(p.success),
                        "New role",
                    )
                    .clicked()
                    {
                        wanted = Some(UsersDiscard::NewRole);
                    }
                    if icons::button(ui, true, icons::REFRESH, None, "Reload")
                        .on_hover_text("Load the roles again")
                        .clicked()
                    {
                        wanted = Some(UsersDiscard::Reload);
                    }
                    if self.roles_job.is_some() {
                        ui.spinner();
                    }
                });
                let mut selected = self.selected.clone();
                if role_list(
                    ui,
                    &self.roles,
                    &mut selected,
                    &mut self.filter,
                    self.current_user.as_deref(),
                ) && let Some(grantee) = selected
                {
                    wanted = Some(UsersDiscard::Select(grantee));
                }
                // Pending edits are only dropped once the user agrees.
                match wanted {
                    Some(action) if self.pending() > 0 => {
                        command = Some(AdminCommand::Discard(action));
                    }
                    Some(action) => self.perform(engine, action),
                    None => {}
                }
            });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.add_space(Spacing::S);
            let Some(editor) = &self.editor else {
                if let Some((text, error)) = &self.message {
                    let color = if *error { p.error } else { p.success };
                    ui.label(RichText::new(text).color(color));
                }
                theme::hint(ui, "Select a role, or create one with New role");
                return;
            };
            let pending = self.pending();
            let busy = self.apply.is_some();
            let existing = editor.before.clone();
            let blocked = super::actions_blocked(engine, &self.conn, true);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = Spacing::S;
                if icons::button(ui, pending > 0 && !busy, icons::SEARCH, None, "Preview").clicked()
                {
                    self.preview = !self.preview;
                }
                if icons::button(
                    ui,
                    pending > 0 && !busy && blocked.is_none(),
                    icons::SUCCESS,
                    Some(p.success),
                    "Apply",
                )
                .on_disabled_hover_text(blocked.unwrap_or(""))
                .clicked()
                {
                    command = Some(AdminCommand::ApplyRole);
                }
                if icons::button(ui, pending > 0 && !busy, icons::CANCELLED, None, "Revert")
                    .clicked()
                {
                    self.revert();
                }
                let can_drop =
                    existing.is_some() && self.support.roles && !busy && blocked.is_none();
                if icons::button(ui, can_drop, icons::CLOSE, Some(p.error), "Drop role…")
                    .on_disabled_hover_text(blocked.unwrap_or(""))
                    .clicked()
                    && let Some(role) = &existing
                {
                    command = Some(AdminCommand::DropRole(role.clone()));
                }
                ui.separator();
                let text = match pending {
                    0 => "No pending changes".to_string(),
                    1 => "1 pending change".to_string(),
                    n => format!("{n} pending changes"),
                };
                let color = if pending > 0 { p.warning } else { p.text_muted };
                ui.label(RichText::new(text).color(color));
                if busy {
                    ui.spinner();
                }
            });
            if let Some((text, error)) = &self.message {
                let color = if *error { p.error } else { p.success };
                ui.label(RichText::new(text).color(color));
            }
            if self.preview {
                let syntax = dialect.map_or_else(SqlSyntax::ansi, |d| d.syntax());
                // How the statements run does not change what they say.
                match self.changes(engine, false) {
                    Ok(Some(c)) => {
                        let shown = c.options.masking.map(|m| m.shown).unwrap_or_default();
                        sql_view(ui, "role_preview", &script(&shown), &syntax, 160.0);
                    }
                    Ok(None) => {
                        theme::hint(ui, "No pending changes");
                    }
                    Err(e) => {
                        ui.colored_label(p.error, e.to_string());
                    }
                }
            }
            ui.add_space(Spacing::S);
            let roles: Vec<String> = self.roles.iter().map(RoleDef::grantee).collect();
            if let Some(editor) = &mut self.editor {
                ui.add_enabled_ui(!busy, |ui| role_form(ui, editor, &self.support, &roles));
            }
            if !self.support.grants {
                return;
            }
            ui.add_space(Spacing::M);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Privileges").strong());
                let schemas = self.schemas(engine, dialect);
                let text = self.schema.clone().unwrap_or_else(|| "No schema".into());
                egui::ComboBox::from_id_salt(("privilege_schema", key))
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        for s in schemas {
                            let label = s.clone();
                            ui.selectable_value(&mut self.schema, Some(s), label);
                        }
                    });
                if self.grants_job.is_some() {
                    ui.spinner();
                }
            });
            let rows = self.privilege_rows(engine);
            ui.add_enabled_ui(!busy, |ui| {
                privilege_grid(ui, &mut self.grid, &rows, &self.support);
            });
            if self.support.hosts {
                // MySQL and MariaDB: the grid has no row for the server-wide level.
                let muted = theme::palette(ui.ctx()).text_muted;
                let text = "Global privileges (ON *.*) are not listed here";
                ui.label(RichText::new(text).small().color(muted))
                    .on_hover_text("Grant or revoke them with GRANT … ON *.* in a query tab");
            }
        });
        command
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sqlr_core::{
        AdminSupport, CatalogNeed, CatalogRelation, ConnectionConfig, ConnectionProfile, Grant,
        ObjectKind, ObjectRef, RelationKind, SchemaCatalog, Secrets,
    };
    use sqlr_engine::{DriverRegistry, Engine, LoadState, SessionStatus};
    use sqlr_testkit::{FakeAdmin, FakeDriver, FakeScript};

    use super::UsersTab;

    fn wait(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn privilege_rows_are_built_once_per_change() {
        let relations = (0..5_000)
            .map(|i| CatalogRelation {
                name: format!("t{i}"),
                kind: RelationKind::Table,
                columns: Vec::new(),
            })
            .collect();
        let support = AdminSupport {
            roles: true,
            grants: true,
            privileges: vec![
                (ObjectKind::Table, vec!["SELECT".into()]),
                (ObjectKind::Sequence, vec!["USAGE".into()]),
            ],
            ..Default::default()
        };
        let fake = FakeDriver::new(FakeScript {
            admin: Some(FakeAdmin {
                support,
                ..Default::default()
            }),
            catalog: vec![SchemaCatalog {
                schema: "s".into(),
                relations,
            }],
            default_schemas: vec!["s".into()],
            ..Default::default()
        });
        let mut drivers = DriverRegistry::default();
        drivers.register(fake);
        let engine = Engine::new(drivers, Arc::new(|| {}));
        let cfg = ConnectionConfig {
            id: "c".into(),
            name: "c".into(),
            driver: "fake".into(),
            group: None,
            color: None,
            values: Default::default(),
            profile: None,
            save_secrets: true,
            agent_access: Default::default(),
        };
        engine.connect(cfg, Secrets::default(), ConnectionProfile::development());
        wait("the connect", || {
            matches!(
                engine.session_status("c"),
                Some(SessionStatus::Connected(_))
            )
        });
        engine.load_catalog("c", CatalogNeed::Schema("s".into()));
        wait("the catalog", || {
            engine
                .catalog("c")
                .is_some_and(|c| matches!(c.schema.get("s"), Some(LoadState::Ready(_))))
        });

        let mut tab = UsersTab::new(&engine, "c");
        tab.schema = Some("s".into());
        let grants: Vec<Grant> = (0..5_000)
            .map(|i| Grant {
                grantee: String::new(),
                object: ObjectRef {
                    kind: ObjectKind::Sequence,
                    database: None,
                    schema: Some("s".into()),
                    name: format!("q{i}"),
                    parent: None,
                    signature: None,
                },
                privilege: "USAGE".into(),
                grantable: false,
            })
            .collect();
        tab.set_loaded(grants);
        let started = Instant::now();
        let rows = tab.privilege_rows(&engine);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(rows.len(), 10_000);
        // Every later frame reuses them.
        for _ in 0..10 {
            assert!(Arc::ptr_eq(&rows, &tab.privilege_rows(&engine)));
        }
        assert_eq!(tab.rows_built, 1);
        // New grants rebuild them.
        tab.set_loaded(Vec::new());
        assert_eq!(tab.privilege_rows(&engine).len(), 5_000);
        assert_eq!(tab.rows_built, 2);
    }
}

#[cfg(test)]
mod role_changes_error_tests {
    use super::*;

    #[test]
    fn role_changes_errors_keep_their_messages() {
        assert_eq!(
            RoleChangesError::Problem("The name is empty".into()).to_string(),
            "The name is empty"
        );
        let driver = RoleChangesError::from(sqlr_core::DriverError::new(
            sqlr_core::ErrorKind::Other,
            "no roles here",
        ));
        assert_eq!(driver.to_string(), "no roles here");
    }
}
