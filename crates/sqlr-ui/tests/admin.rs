//! Administration widgets: the privilege grid, the sessions table and
//! its filters, the blocking tree, the role form and the statistics' byte sizes.

use egui::accesskit::Role;
use egui::{Event, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use sqlr_core::{
    AdminSupport, Grant, LockInfo, ObjectKind, ObjectRef, PASSWORD_MASK, RoleDef, RoleField,
    SessionInfo,
};
use sqlr_ui::{
    PrivilegeCell, PrivilegeGridState, PrivilegeRow, RoleEditorState, SessionsAction,
    SessionsState, blocking_tree, human_bytes, privilege_grid, role_form, sessions_table,
};

const PRIVILEGES: [&str; 7] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "TRUNCATE",
    "REFERENCES",
    "TRIGGER",
];

fn table(name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::Table,
        database: None,
        schema: Some("public".into()),
        name: name.into(),
        parent: None,
        signature: None,
    }
}

fn support() -> AdminSupport {
    AdminSupport {
        roles: true,
        grants: true,
        privileges: vec![(
            ObjectKind::Table,
            PRIVILEGES.iter().map(|p| p.to_string()).collect(),
        )],
        role_fields: vec![
            RoleField::Login,
            RoleField::Superuser,
            RoleField::MemberOf,
            RoleField::Password,
        ],
        ..Default::default()
    }
}

struct GridApp {
    state: PrivilegeGridState,
    rows: Vec<PrivilegeRow>,
    support: AdminSupport,
}

#[test]
fn privilege_grid_virtualized() {
    let rows: Vec<PrivilegeRow> = (0..2000)
        .map(|i| PrivilegeRow {
            grantee: "alice".into(),
            object: table(&format!("t{i}")),
            label: format!("public.t{i}"),
        })
        .collect();
    let mut state = PrivilegeGridState::default();
    state.set_grants(&[Grant {
        grantee: "alice".into(),
        object: table("t1"),
        privilege: "SELECT".into(),
        grantable: true,
    }]);
    let app = GridApp {
        state,
        rows,
        support: support(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1200.0, 700.0))
        .build_ui_state(
            |ui, app: &mut GridApp| {
                privilege_grid(ui, &mut app.state, &app.rows, &app.support);
            },
            app,
        );
    let started = std::time::Instant::now();
    h.run_steps(2);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    h.get_by_label("2000 rows");
    for privilege in PRIVILEGES {
        h.get_by_label(privilege);
    }
    // Only the visible rows are built.
    assert!(h.query_by_label("public.t10").is_some());
    assert!(h.query_by_label("public.t1990").is_none());
    assert_eq!(h.state().state.pending(), 0);

    // none → granted: a pending GRANT.
    h.get_by_label("SELECT on public.t0").click();
    h.run_steps(2);
    let alice_t0 = |privilege: &str, grantable| Grant {
        grantee: "alice".into(),
        object: table("t0"),
        privilege: privilege.into(),
        grantable,
    };
    assert_eq!(
        h.state().state.changes(),
        (vec![alice_t0("SELECT", false)], vec![])
    );
    h.get_by_label("1 pending change");
    // granted → grantable → none: back to nothing pending.
    h.get_by_label("SELECT on public.t0").click();
    h.run_steps(2);
    assert_eq!(
        h.state().state.changes(),
        (vec![alice_t0("SELECT", true)], vec![])
    );
    h.get_by_label("SELECT on public.t0").click();
    h.run_steps(2);
    assert_eq!(h.state().state.pending(), 0);

    // grantable → none on t1 is a REVOKE.
    h.get_by_label("SELECT on public.t1").click();
    h.run_steps(2);
    let (add, revoke) = h.state().state.changes();
    assert!(add.is_empty());
    // It was the object's last grantable privilege: its grant option goes too.
    let alice_t1 = |grantable| Grant {
        grantee: "alice".into(),
        object: table("t1"),
        privilege: "SELECT".into(),
        grantable,
    };
    assert_eq!(revoke, [alice_t1(false), alice_t1(true)]);
    assert_eq!(
        h.state().state.cell("alice", &table("t1"), "SELECT"),
        PrivilegeCell::None
    );

    // Far rows are reachable.
    h.state_mut().state.scroll_to = Some(1990);
    h.run_steps(3);
    assert!(h.query_by_label("public.t1990").is_some());
    assert!(h.query_by_label("public.t10").is_none());

    // The filter narrows the rows.
    h.state_mut().state.filter = "t1999".into();
    h.run_steps(2);
    h.get_by_label("1 of 2000 rows");
}

#[test]
fn revoking_a_grantable_privilege_keeps_the_option_while_another_needs_it() {
    let g = |privilege: &str, grantable| Grant {
        grantee: "alice".into(),
        object: table("t"),
        privilege: privilege.into(),
        grantable,
    };
    let mut state = PrivilegeGridState::default();
    state.set_grants(&[g("SELECT", true), g("UPDATE", true)]);
    state.set("alice", &table("t"), "SELECT", PrivilegeCell::None);
    assert_eq!(state.changes(), (vec![], vec![g("SELECT", false)]));
    state.set("alice", &table("t"), "UPDATE", PrivilegeCell::None);
    let (_, revoke) = state.changes();
    assert!(revoke.contains(&g("SELECT", false)) && revoke.contains(&g("UPDATE", false)));
    assert!(revoke.iter().any(|r| r.grantable), "{revoke:?}");
}

struct SessionsApp {
    state: SessionsState,
    sessions: Vec<SessionInfo>,
    actions: Vec<SessionsAction>,
}

fn session(id: &str, user: &str, db: &str, state: &str, own: bool) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        user: user.into(),
        database: Some(db.into()),
        client: Some("psql".into()),
        state: state.into(),
        query: Some(format!("select {id}")),
        own,
        ..Default::default()
    }
}

fn sessions_harness(sessions: Vec<SessionInfo>) -> Harness<'static, SessionsApp> {
    let app = SessionsApp {
        state: SessionsState::default(),
        sessions,
        actions: Vec::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(1200.0, 500.0))
        .build_ui_state(
            |ui, app: &mut SessionsApp| {
                if let Some(a) = sessions_table(ui, &mut app.state, &app.sessions) {
                    app.actions.push(a);
                }
            },
            app,
        );
    h.run_steps(2);
    h
}

#[test]
fn own_session_marked() {
    let mut h = sessions_harness(vec![
        session("10", "app", "shop", "active", false),
        session("11", "me", "shop", "idle", true),
        session("12", "app", "shop", "idle", false),
    ]);
    assert_eq!(h.query_all_by_label("this connection").count(), 1);
    // Nothing selected: no action.
    assert!(h.get_by_label("Terminate").accesskit_node().is_disabled());
    h.get_by_label("11").click();
    h.run_steps(2);
    assert_eq!(h.state().state.selected.as_deref(), Some("11"));
    h.get_by_label("Terminate").click();
    h.run_steps(2);
    h.get_by_label("Cancel query").click();
    h.run_steps(2);
    assert_eq!(
        h.state().actions,
        [
            SessionsAction::Terminate("11".into()),
            SessionsAction::Cancel("11".into())
        ]
    );
}

#[test]
fn sessions_filters() {
    let sessions = vec![
        session("1", "alice", "shop", "active", false),
        session("2", "alice", "crm", "idle", false),
        session("3", "bob", "shop", "idle", true),
        session("4", "bob", "crm", "active", false),
    ];
    let ids = |state: &SessionsState| -> Vec<String> {
        state
            .visible(&sessions)
            .iter()
            .map(|s| s.id.clone())
            .collect()
    };
    let mut state = SessionsState::default();
    assert_eq!(state.interval_s, 2, "two seconds by default");
    assert_eq!(ids(&state), ["1", "2", "3", "4"]);
    state.hide_idle = true;
    assert_eq!(ids(&state), ["1", "4"]);
    state.hide_idle = false;
    state.user = Some("bob".into());
    assert_eq!(ids(&state), ["3", "4"]);
    state.database = Some("crm".into());
    assert_eq!(ids(&state), ["4"]);

    // Through the widget: "Hide idle" hides the idle rows.
    let mut h = sessions_harness(sessions.clone());
    assert!(h.query_by_label("2").is_some());
    h.get_by_label("Hide idle").click();
    h.run_steps(2);
    assert!(h.state().state.hide_idle);
    assert!(h.query_by_label("2").is_none() && h.query_by_label("3").is_none());
    assert!(h.query_by_label("1").is_some() && h.query_by_label("4").is_some());
    h.get_by_label("2 of 4 sessions");
}

#[test]
fn blocking_tree_indents_waiters_under_blockers() {
    let lock = |session: &str, blocked_by: &[&str], granted| LockInfo {
        session: session.into(),
        blocked_by: blocked_by.iter().map(|s| s.to_string()).collect(),
        mode: "ExclusiveLock".into(),
        object: Some("t".into()),
        granted,
    };
    // 1 blocks 2, 2 blocks 3; 4 waits on 1 too; 5 holds a lock nobody waits for.
    let locks = [
        lock("1", &[], true),
        lock("2", &[], true),
        lock("2", &["1"], false),
        lock("3", &["2"], false),
        lock("4", &["1"], false),
        lock("5", &[], true),
    ];
    assert_eq!(
        blocking_tree(&locks),
        [
            (0, "1".to_string()),
            (1, "2".to_string()),
            (2, "3".to_string()),
            (1, "4".to_string()),
        ]
    );
    // A cycle (a deadlock about to be broken) ends instead of looping.
    let cycle = [lock("1", &["2"], false), lock("2", &["1"], false)];
    assert!(blocking_tree(&cycle).len() <= 2);
}

struct RoleApp {
    state: RoleEditorState,
    support: AdminSupport,
}

#[test]
fn role_form_edits_the_role_and_masks_the_password() {
    let app = RoleApp {
        state: RoleEditorState::new_role(false),
        support: support(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 600.0))
        .build_ui_state(
            |ui, app: &mut RoleApp| {
                role_form(ui, &mut app.state, &app.support, &["reader".to_string()]);
            },
            app,
        );
    h.run_steps(2);
    assert!(h.state().state.is_new() && h.state().state.pending());
    h.get_by_label("Superuser").click();
    h.run_steps(2);
    assert!(h.state().state.after.superuser);
    // Only the fields the driver supports.
    assert!(h.query_by_label("Replication").is_none());
    assert!(h.query_by_label("Host").is_none());
    h.get_by_label("reader").click();
    h.run_steps(2);
    assert_eq!(h.state().state.after.member_of, ["reader"]);

    let password = By::new().role(Role::PasswordInput);
    h.get(password).click();
    h.run_steps(2);
    h.event(Event::Text("s3cret".into()));
    h.run_steps(2);
    let after = &h.state().state.after;
    assert_eq!(after.password.as_ref().unwrap().expose(), "s3cret");
    assert!(!format!("{:?}", h.state().state.after).contains("s3cret"));
    assert_eq!(
        h.state().state.after.masked().password.unwrap().expose(),
        PASSWORD_MASK
    );

    // An existing role: nothing pending until edited.
    let mut existing = RoleEditorState::new(RoleDef {
        name: "reader".into(),
        ..Default::default()
    });
    assert!(!existing.is_new() && !existing.pending());
    existing.after.login = true;
    assert!(existing.pending());
    existing.revert();
    assert!(!existing.pending());
}

fn role_form_harness(state: RoleEditorState, support: AdminSupport) -> Harness<'static, RoleApp> {
    let mut h = Harness::builder()
        .with_size(vec2(800.0, 600.0))
        .build_ui_state(
            |ui, app: &mut RoleApp| {
                let roles = ["reader".to_string(), "pg_read_all_data".to_string()];
                role_form(ui, &mut app.state, &app.support, &roles);
            },
            RoleApp { state, support },
        );
    h.run_steps(2);
    h
}

#[test]
fn host_less_mysql_roles_have_no_password_or_limit() {
    let mut support = support();
    support.hosts = true;
    support.role_fields.push(RoleField::ConnectionLimit);
    let mut role = RoleEditorState::new_role(true);
    role.after.host = None;
    let h = role_form_harness(role, support.clone());
    assert!(h.query(By::new().role(Role::PasswordInput)).is_none());
    assert!(h.query_by_label("Connection limit").is_none());
    // An account (with a host) has both.
    let h = role_form_harness(RoleEditorState::new_role(true), support);
    assert!(h.query(By::new().role(Role::PasswordInput)).is_some());
    h.get_by_label("Connection limit");
}

#[test]
fn a_password_can_be_removed_and_predefined_roles_joined() {
    let role = RoleEditorState::new(RoleDef {
        name: "app".into(),
        login: true,
        ..Default::default()
    });
    let mut h = role_form_harness(role, support());
    h.get_by_label("Remove the password").click();
    h.run_steps(2);
    assert!(h.state().state.after.clear_password);
    assert!(h.state().state.pending());
    h.get_by_label("pg_read_all_data").click();
    h.run_steps(2);
    assert_eq!(h.state().state.after.member_of, ["pg_read_all_data"]);
}

#[test]
fn predefined_roles_are_not_listed() {
    struct ListApp {
        roles: Vec<RoleDef>,
        selected: Option<String>,
        filter: String,
    }
    let predefined = RoleDef {
        name: "pg_monitor".into(),
        options: vec![("predefined".into(), "true".into())],
        ..Default::default()
    };
    let app = ListApp {
        roles: vec![
            RoleDef {
                name: "app".into(),
                ..Default::default()
            },
            predefined,
        ],
        selected: None,
        filter: String::new(),
    };
    let mut h = Harness::builder()
        .with_size(vec2(400.0, 400.0))
        .build_ui_state(
            |ui, app: &mut ListApp| {
                sqlr_ui::role_list(ui, &app.roles, &mut app.selected, &mut app.filter, None);
            },
            app,
        );
    h.run_steps(2);
    h.get_by_label("app");
    assert!(h.query_by_label("pg_monitor").is_none());
}

#[test]
fn byte_sizes_are_human_readable() {
    assert_eq!(human_bytes(0), "0 B");
    assert_eq!(human_bytes(1023), "1023 B");
    assert_eq!(human_bytes(1536), "1.5 KB");
    assert_eq!(human_bytes(8192), "8.0 KB");
    assert_eq!(human_bytes(20 * 1024 * 1024), "20 MB");
    assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
}
