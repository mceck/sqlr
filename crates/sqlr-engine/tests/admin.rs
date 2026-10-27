//! Administration jobs: snapshots of roles, grants, sessions, locks,
//! properties and statistics; cancel and terminate; the dialect's role and grant DDL; and
//! applies whose statements hold a password.

use std::sync::Arc;

use sqlr_core::{
    AdminSupport, ConnectionProfile, ErrorKind, Grant, LockInfo, ObjectKind, ObjectRef, Password,
    RoleDef, Secrets, ServerProperty, SessionInfo, TableRef, TableStats,
};
use sqlr_engine::{
    AdminAction, AdminQuery, AdminSnapshot, ApplyOptions, DdlStepOutcome, DriverRegistry, Engine,
    JobId, JobKind, JobOutcome, Masking, SessionKey, SessionStatus, Slot,
};
use sqlr_testkit::{FakeAdmin, FakeDriver, FakeScript};

use crate::support;
use support::{TIMEOUT, config, dev, no_repaint, wait_until};

fn session(id: &str, user: &str, state: &str) -> SessionInfo {
    SessionInfo {
        id: id.into(),
        user: user.into(),
        state: state.into(),
        ..Default::default()
    }
}

fn table(name: &str) -> ObjectRef {
    ObjectRef {
        kind: ObjectKind::Table,
        database: None,
        schema: Some("s".into()),
        name: name.into(),
        parent: None,
        signature: None,
    }
}

fn fake_admin() -> FakeAdmin {
    FakeAdmin {
        support: AdminSupport {
            roles: true,
            grants: true,
            sessions: true,
            locks: true,
            properties: true,
            statistics: true,
            privileges: vec![(ObjectKind::Table, vec!["SELECT".into(), "INSERT".into()])],
            ..Default::default()
        },
        roles: vec![RoleDef {
            name: "reader".into(),
            ..Default::default()
        }],
        grants: vec![Grant {
            grantee: "reader".into(),
            object: table("orders"),
            privilege: "SELECT".into(),
            grantable: false,
        }],
        // The explorer session's fake connection is number 1, the next one 2.
        sessions: vec![
            session("1", "me", "idle"),
            session("2", "me", "idle"),
            session("3", "other", "active"),
        ],
        locks: vec![LockInfo {
            session: "3".into(),
            blocked_by: vec!["4".into()],
            mode: "ShareLock".into(),
            object: None,
            granted: false,
        }],
        properties: vec![ServerProperty {
            name: "max_connections".into(),
            value: "100".into(),
            ..Default::default()
        }],
        sizes: vec![("app".into(), 8192)],
        stats: vec![TableStats {
            table: TableRef {
                database: None,
                schema: Some("s".into()),
                name: "orders".into(),
            },
            rows_estimate: Some(3),
            total_bytes: Some(16384),
            table_bytes: Some(8192),
            index_bytes: Some(8192),
            extra: Vec::new(),
        }],
        current_user: Some("me".into()),
    }
}

fn setup(script: FakeScript, profile: ConnectionProfile) -> (Engine, Arc<FakeDriver>) {
    let fake = FakeDriver::new(script);
    let mut drivers = DriverRegistry::default();
    drivers.register(fake.clone());
    let engine = Engine::new(drivers, no_repaint());
    engine.connect(config("c", "fake"), Secrets::default(), profile);
    assert!(wait_until(TIMEOUT, || matches!(
        engine.session_status("c"),
        Some(SessionStatus::Connected(_))
    )));
    (engine, fake)
}

fn admin_script() -> FakeScript {
    FakeScript {
        admin: Some(fake_admin()),
        ..Default::default()
    }
}

fn outcome(engine: &Engine, id: JobId) -> JobOutcome {
    assert!(wait_until(TIMEOUT, || engine
        .job(id)
        .is_some_and(|j| j.outcome.is_some())));
    let state = engine.job(id).unwrap();
    engine.forget_job(id);
    state.outcome.unwrap()
}

fn snapshot(engine: &Engine, query: AdminQuery) -> AdminSnapshot {
    let id = engine.admin_snapshot("c", query);
    assert_eq!(engine.job(id).map(|j| j.kind), Some(JobKind::AdminSnapshot));
    match outcome(engine, id) {
        JobOutcome::Admin(snapshot) => snapshot,
        other => panic!("{other:?}"),
    }
}

#[test]
fn admin_snapshots_answer_each_query() {
    let (engine, fake) = setup(admin_script(), dev());
    let expected = fake_admin();
    assert_eq!(engine.admin_support("c"), Some(expected.support.clone()));
    assert_eq!(engine.admin_current_user("c").as_deref(), Some("me"));
    assert_eq!(engine.admin_support("missing"), None);

    assert_eq!(
        snapshot(&engine, AdminQuery::Roles),
        AdminSnapshot::Roles(expected.roles.clone())
    );
    let grants = AdminQuery::Grants {
        grantee: Some("reader".into()),
        object: None,
    };
    assert_eq!(
        snapshot(&engine, grants),
        AdminSnapshot::Grants(expected.grants.clone())
    );
    assert_eq!(
        snapshot(&engine, AdminQuery::Locks),
        AdminSnapshot::Locks(expected.locks.clone())
    );
    assert_eq!(
        snapshot(&engine, AdminQuery::Properties),
        AdminSnapshot::Properties(expected.properties.clone())
    );
    assert_eq!(
        snapshot(&engine, AdminQuery::DatabaseSizes),
        AdminSnapshot::DatabaseSizes(expected.sizes.clone())
    );
    let stats = AdminQuery::TableStats {
        schema: Some("s".into()),
    };
    assert_eq!(
        snapshot(&engine, stats),
        AdminSnapshot::TableStats(expected.stats)
    );
    assert_eq!(
        fake.admin_calls(),
        [
            "roles",
            "grants reader",
            "locks",
            "properties",
            "sizes",
            "stats s"
        ]
    );

    // A connection that is not connected fails at once.
    let id = engine.admin_snapshot("missing", AdminQuery::Roles);
    match outcome(&engine, id) {
        JobOutcome::Failed(e) => assert_eq!(e.kind, ErrorKind::Connection),
        other => panic!("{other:?}"),
    }
}

#[test]
fn own_sessions_include_every_session_of_the_app() {
    let (engine, _fake) = setup(admin_script(), dev());
    // Without a tab session, only the explorer session's connection is the app's.
    let AdminSnapshot::Sessions(sessions) = snapshot(&engine, AdminQuery::Sessions) else {
        unreachable!()
    };
    let own: Vec<&str> = sessions
        .iter()
        .filter(|s| s.own)
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(own, ["1"]);

    // A tab's session (connection 2) is the app's too.
    let tab = SessionKey {
        conn: "c".into(),
        slot: Slot::Tab(7),
    };
    let q = engine.execute_in(&tab, None, "select 1".into(), 10);
    assert!(wait_until(TIMEOUT, || engine
        .result(q)
        .is_some_and(
            |r| r.read().status != sqlr_engine::QueryStatus::Running
        )));
    let AdminSnapshot::Sessions(sessions) = snapshot(&engine, AdminQuery::Sessions) else {
        unreachable!()
    };
    let own: Vec<&str> = sessions
        .iter()
        .filter(|s| s.own)
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(own, ["1", "2"]);
    assert!(!sessions[2].own);
}

#[test]
fn cancel_and_terminate_are_jobs_refused_when_read_only() {
    let (engine, fake) = setup(admin_script(), dev());
    for (action, call) in [
        (
            AdminAction::CancelSession {
                id: "3".into(),
                started_ms: Some(42),
            },
            "cancel 3 started 42",
        ),
        (
            AdminAction::TerminateSession {
                id: "3".into(),
                started_ms: None,
            },
            "terminate 3",
        ),
    ] {
        let id = engine.admin_action("c", action);
        assert_eq!(engine.job(id).map(|j| j.kind), Some(JobKind::AdminAction));
        assert_eq!(outcome(&engine, id), JobOutcome::Done);
        assert_eq!(fake.admin_calls().last().map(String::as_str), Some(call));
    }

    let read_only = ConnectionProfile {
        read_only: true,
        ..dev()
    };
    let (engine, fake) = setup(admin_script(), read_only);
    let terminate = AdminAction::TerminateSession {
        id: "3".into(),
        started_ms: None,
    };
    let id = engine.admin_action("c", terminate);
    match outcome(&engine, id) {
        JobOutcome::Failed(e) => assert_eq!(e.kind, ErrorKind::Permission),
        other => panic!("{other:?}"),
    }
    assert!(fake.admin_calls().is_empty());
}

#[test]
fn role_and_grant_ddl_come_from_the_dialect() {
    let (engine, _fake) = setup(admin_script(), dev());
    let role = RoleDef {
        name: "app".into(),
        login: true,
        password: Some(Password::new("s3cret")),
        ..Default::default()
    };
    assert_eq!(
        engine.role_ddl("c", None, &role).unwrap(),
        ["CREATE ROLE app LOGIN PASSWORD 's3cret'"]
    );
    assert_eq!(
        engine.role_ddl("c", None, &role.masked()).unwrap(),
        ["CREATE ROLE app LOGIN PASSWORD '********'"]
    );
    assert_eq!(engine.drop_role_ddl("c", &role).unwrap(), ["DROP ROLE app"]);
    let select = Grant {
        grantee: "app".into(),
        object: table("orders"),
        privilege: "SELECT".into(),
        grantable: true,
    };
    assert_eq!(
        engine
            .grant_ddl("c", std::slice::from_ref(&select), &[])
            .unwrap(),
        ["GRANT SELECT ON s.orders TO app WITH GRANT OPTION"]
    );
    assert_eq!(
        engine.grant_ddl("c", &[], &[select]).unwrap(),
        ["REVOKE GRANT OPTION FOR SELECT ON s.orders FROM app"]
    );
    assert_eq!(
        engine.role_ddl("missing", None, &role).unwrap_err().kind,
        ErrorKind::Connection
    );
}

#[test]
fn applies_show_masked_statements_and_errors() {
    let (engine, fake) = setup(
        FakeScript {
            fail_matching: Some("s3cret".into()),
            ..admin_script()
        },
        dev(),
    );
    let real = vec![
        "GRANT reader TO app".to_string(),
        "ALTER ROLE app PASSWORD 's3cret'".to_string(),
    ];
    let shown = vec![
        "GRANT reader TO app".to_string(),
        "ALTER ROLE app PASSWORD '********'".to_string(),
    ];
    let options = ApplyOptions {
        transactional: true,
        masking: Some(Masking {
            shown: shown.clone(),
            secrets: vec![Password::new("s3cret")],
        }),
        ..Default::default()
    };
    let printed = format!("{options:?}");
    assert!(!printed.contains("s3cret"), "{printed}");
    let id = engine.apply_ddl(&SessionKey::meta("c"), real.clone(), options);
    let JobOutcome::DdlApplied(report) = outcome(&engine, id) else {
        panic!("not applied")
    };
    // The server got the password; the report never holds it.
    assert_eq!(fake.executed(), ["BEGIN", &real[0], &real[1], "ROLLBACK"]);
    let texts: Vec<&str> = report.statements.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(texts, shown);
    let DdlStepOutcome::Failed(e) = &report.statements[1].1 else {
        panic!("{report:?}")
    };
    assert!(!e.message.contains("s3cret") && e.message.contains("********"));
    assert!(!report.summary().contains("s3cret"));
    assert!(!format!("{report:?}").contains("s3cret"));
}

#[test]
fn errors_lose_the_escaped_forms_of_a_password() {
    // The statement holds the password as the dialect escaped it (quotes and backslashes
    // doubled), and the server's error echoes that part of the statement.
    let password = "it's \\ x";
    let escaped = "it''s \\\\ x";
    let (engine, _fake) = setup(
        FakeScript {
            fail_matching: Some(escaped.into()),
            ..admin_script()
        },
        dev(),
    );
    let real = vec![format!("ALTER ROLE app PASSWORD E'{escaped}'")];
    let options = ApplyOptions {
        transactional: false,
        masking: Some(Masking {
            shown: vec!["ALTER ROLE app PASSWORD '********'".into()],
            secrets: vec![Password::new(password)],
        }),
        ..Default::default()
    };
    let id = engine.apply_ddl(&SessionKey::meta("c"), real, options);
    let JobOutcome::DdlApplied(report) = outcome(&engine, id) else {
        panic!("not applied")
    };
    let summary = report.summary();
    assert!(summary.contains("********"), "{summary}");
    for leak in [password, escaped, "it''s", "\\\\ x"] {
        assert!(!summary.contains(leak), "{leak} in {summary}");
    }
}
