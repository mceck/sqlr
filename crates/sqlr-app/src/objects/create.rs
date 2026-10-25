//! "New ▸ <kind>": a dialog asks for the name (and the table of an index or trigger), then
//! an object tab opens in "new" mode, where the rest is edited in the same forms.

use egui::{ComboBox, TextEdit};
use sqlr_core::{
    DatabaseDef, IndexDef, NodePath, ObjectDef, ObjectKind, RoutineDef, SchemaDef, SequenceDef,
    TableDef, TableRef, TriggerDef, ViewDef,
};
use sqlr_ui::kind_label;
use sqlr_ui::text_edit;
use sqlr_ui::theme::{self};

pub(crate) struct CreateDialog {
    pub conn: String,
    pub kind: ObjectKind,
    /// Where the object goes.
    pub schema: Option<String>,
    pub database: Option<String>,
    pub name: String,
    /// The table of an index or trigger.
    pub table: String,
    /// Tables to choose from (the schema's, from the catalog).
    pub tables: Vec<String>,
    /// Explorer paths to reload once it is created.
    pub refresh: Vec<NodePath>,
    /// The driver's dialect id, for sensible routine defaults.
    pub dialect: String,
    focused: bool,
}

impl CreateDialog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conn: &str,
        kind: ObjectKind,
        schema: Option<String>,
        database: Option<String>,
        table: Option<String>,
        tables: Vec<String>,
        refresh: Vec<NodePath>,
        dialect: &str,
    ) -> Self {
        Self {
            conn: conn.to_string(),
            kind,
            schema,
            database,
            name: String::new(),
            table: table.unwrap_or_default(),
            tables,
            refresh,
            dialect: dialect.to_string(),
            focused: false,
        }
    }

    fn needs_table(&self) -> bool {
        matches!(self.kind, ObjectKind::Index | ObjectKind::Trigger)
    }

    /// Why Create is disabled, if it is.
    fn blocked(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() {
            Some("Enter a name")
        } else if self.needs_table() && self.table.is_empty() {
            Some("Choose a table")
        } else {
            None
        }
    }

    /// Draws the dialog: `Some(true)` to create, `Some(false)` to cancel.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<bool> {
        let title = format!("New {}", kind_label(self.kind).to_lowercase());
        let subtitle = self.schema.as_ref().map(|s| format!("In {s}"));
        let mut dialog =
            theme::Dialog::new("create_object", theme::icons::CREATE, &title).width(420.0);
        if let Some(subtitle) = &subtitle {
            dialog = dialog.subtitle(subtitle);
        }
        let response = dialog.show(ctx, |d| {
            d.body(|ui| self.form(ui));
            let blocked = self.blocked();
            d.footer(|f| {
                let create = f.primary("Create", blocked);
                f.cancel();
                create
            })
        });
        if response.inner {
            Some(true)
        } else if response.closed {
            Some(false)
        } else {
            None
        }
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        {
            let label = ui.label("Name");
            let field = text_edit(
                ui,
                TextEdit::singleline(&mut self.name).desired_width(f32::INFINITY),
            )
            .labelled_by(label.id);
            if !std::mem::replace(&mut self.focused, true) {
                field.request_focus();
            }
            if self.needs_table() {
                theme::field_label(ui, "Table");
                let text = if self.table.is_empty() {
                    "Choose a table…".to_string()
                } else {
                    self.table.clone()
                };
                ComboBox::from_id_salt("create_object_table")
                    .selected_text(text)
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        if self.tables.is_empty() {
                            ui.label("Loading tables…");
                        }
                        for t in &self.tables {
                            ui.selectable_value(&mut self.table, t.clone(), t);
                        }
                    });
            }
        }
    }

    /// The definition the object tab starts from; `None` for roles, which are created in
    /// the administration panels.
    pub fn definition(&self) -> Option<ObjectDef> {
        let name = self.name.trim().to_string();
        let relation = |name: &str| TableRef {
            database: None,
            schema: self.schema.clone(),
            name: name.to_string(),
        };
        let postgres = self.dialect == "postgres";
        Some(match self.kind {
            ObjectKind::Table => ObjectDef::Table(TableDef::new(relation(&name))),
            ObjectKind::View | ObjectKind::MaterializedView => ObjectDef::View(ViewDef {
                view: relation(&name),
                query: "SELECT 1 AS one".into(),
                materialized: self.kind == ObjectKind::MaterializedView,
                columns: Vec::new(),
                comment: None,
                options: Vec::new(),
                indexes: Vec::new(),
            }),
            ObjectKind::Sequence => ObjectDef::Sequence(SequenceDef {
                sequence: relation(&name),
                data_type: None,
                start: None,
                increment: None,
                min_value: None,
                max_value: None,
                cache: None,
                cycle: false,
                comment: None,
            }),
            ObjectKind::Function | ObjectKind::Procedure => {
                let function = self.kind == ObjectKind::Function;
                let (returns, language, body) = match (postgres, function) {
                    (true, true) => (Some("integer"), Some("sql"), "SELECT 1"),
                    (true, false) => (None, Some("sql"), "SELECT 1"),
                    (false, true) => (Some("INT"), None, "RETURN 1"),
                    (false, false) => (None, None, "BEGIN\nEND"),
                };
                ObjectDef::Routine(RoutineDef {
                    schema: self.schema.clone(),
                    name,
                    kind: self.kind,
                    arguments: String::new(),
                    identity_arguments: None,
                    returns: returns.map(String::from),
                    language: language.map(String::from),
                    body: body.into(),
                    options: Vec::new(),
                })
            }
            ObjectKind::Index => ObjectDef::Index {
                table: relation(&self.table),
                index: IndexDef {
                    name,
                    ..Default::default()
                },
            },
            ObjectKind::Trigger => ObjectDef::Trigger(TriggerDef {
                name,
                table: relation(&self.table),
                timing: "BEFORE".into(),
                events: vec!["INSERT".into()],
                for_each_row: true,
                condition: None,
                body: String::new(),
            }),
            ObjectKind::Schema => ObjectDef::Schema(SchemaDef {
                name,
                owner: None,
                comment: None,
            }),
            ObjectKind::Database => ObjectDef::Database(DatabaseDef {
                name,
                options: Vec::new(),
            }),
            ObjectKind::Role => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(kind: ObjectKind) -> CreateDialog {
        let mut d = CreateDialog::new(
            "c",
            kind,
            Some("s".into()),
            None,
            None,
            Vec::new(),
            Vec::new(),
            "postgres",
        );
        d.name = "x".into();
        d
    }

    #[test]
    fn roles_are_not_created_here() {
        assert!(dialog(ObjectKind::Role).definition().is_none());
        assert!(matches!(
            dialog(ObjectKind::Database).definition(),
            Some(ObjectDef::Database(_))
        ));
    }
}
