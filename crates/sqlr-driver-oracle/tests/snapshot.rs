//! A whole schema read from Oracle Free, and the `MERGE` the dialect writes for an upsert.

mod support;

use sqlr_core::{
    ConstraintDef, Dialect, Execution, FkAction, ObjectDef, ObjectKind, ObjectRef, Value,
};
use sqlr_driver_oracle::OracleDialect;
use sqlr_testkit::unique_name;

#[tokio::test]
async fn a_schema_loads_with_its_keys_indexes_and_views() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let rel = conn.relational().unwrap();
    let base = unique_name("SQLR_SNAP").to_uppercase();
    let (customers, orders, view) = (
        format!("{base}_C"),
        format!("{base}_O"),
        format!("{base}_V"),
    );
    for sql in [
        format!(
            "CREATE TABLE {customers} (id NUMBER(10) NOT NULL, email VARCHAR2(120) NOT NULL, \
             age NUMBER(3), CONSTRAINT {base}_PK PRIMARY KEY (id), \
             CONSTRAINT {base}_UQ UNIQUE (email), CONSTRAINT {base}_CK CHECK (age >= 0))"
        ),
        format!(
            "CREATE TABLE {orders} (id NUMBER(10) PRIMARY KEY, customer_id NUMBER(10) NOT NULL, \
             total NUMBER(10,2) NOT NULL, note VARCHAR2(50), \
             CONSTRAINT {base}_FK FOREIGN KEY (customer_id) REFERENCES {customers} (id) \
             ON DELETE CASCADE)"
        ),
        format!("CREATE INDEX {base}_IX ON {orders} (total DESC, note)"),
        format!("CREATE VIEW {view} AS SELECT id, total FROM {orders} WHERE total > 100"),
    ] {
        rel.execute(&sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e:?}"));
    }
    let snapshot = rel
        .introspect()
        .expect("introspection")
        .load_schema(None, "SQLR_TEST")
        .await;
    let drop_all = || async {
        let _ = rel.execute(&format!("DROP VIEW {view}")).await;
        let _ = rel.execute(&format!("DROP TABLE {orders} PURGE")).await;
        let _ = rel.execute(&format!("DROP TABLE {customers} PURGE")).await;
    };
    let snapshot = match snapshot {
        Ok(s) => s,
        Err(e) => {
            drop_all().await;
            panic!("{e:?}");
        }
    };
    let table = |name: &str| {
        snapshot
            .objects
            .iter()
            .find_map(|o| match o {
                ObjectDef::Table(t) if t.table.name == name => Some(t.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("table {name}"))
    };

    let c = table(&customers);
    let names: Vec<_> = c.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["ID", "EMAIL", "AGE"]);
    assert_eq!(c.columns[1].type_name, "VARCHAR2(120)");
    assert!(!c.columns[0].nullable && c.columns[2].nullable);
    assert_eq!(c.primary_key(), Some(&["ID".to_string()][..]));
    assert!(c.constraints.iter().any(|k| matches!(
        k, ConstraintDef::Unique { columns, .. } if columns == &["EMAIL".to_string()])));
    assert!(c.constraints.iter().any(|k| matches!(
        k, ConstraintDef::Check { expression, .. } if expression.contains("age >= 0"))));
    // A column's NOT NULL is not a check constraint.
    assert!(!c.constraints.iter().any(|k| matches!(
        k, ConstraintDef::Check { expression, .. } if expression.contains("IS NOT NULL"))));

    let o = table(&orders);
    let fk = o
        .constraints
        .iter()
        .find_map(|k| match k {
            ConstraintDef::ForeignKey {
                columns,
                target,
                target_columns,
                on_delete,
                ..
            } => Some((
                columns.clone(),
                target.clone(),
                target_columns.clone(),
                *on_delete,
            )),
            _ => None,
        })
        .expect("foreign key");
    assert_eq!(fk.0, ["CUSTOMER_ID"]);
    assert_eq!(fk.1.name, customers);
    assert_eq!(fk.1.schema.as_deref(), Some("SQLR_TEST"));
    assert_eq!(fk.2, ["ID"]);
    assert_eq!(fk.3, FkAction::Cascade);
    assert_eq!(o.columns[2].type_name, "NUMBER(10,2)");
    let ix = o
        .indexes
        .iter()
        .find(|i| i.name == format!("{base}_IX"))
        .expect("index");
    assert_eq!(ix.columns.len(), 2);
    assert!(ix.columns[0].desc && !ix.columns[1].desc);
    // Primary-key and unique-constraint indexes belong to their constraints.
    assert!(o.indexes.iter().all(|i| i.name != format!("{base}_PK")));

    let v = snapshot
        .objects
        .iter()
        .find_map(|o| match o {
            ObjectDef::View(v) if v.view.name == view => Some(v.clone()),
            _ => None,
        })
        .expect("view");
    assert!(v.query.contains("total > 100"), "{}", v.query);
    assert_eq!(v.columns, ["ID", "TOTAL"]);

    // The autocomplete catalog and the explorer's column nodes read the same lengths.
    let catalog = rel.catalog("SQLR_TEST").await;
    let email = catalog.as_ref().ok().and_then(|c| {
        c.relations
            .iter()
            .find(|r| r.name == customers)?
            .columns
            .iter()
            .find(|c| c.name == "EMAIL")
            .map(|c| c.type_name.clone())
    });
    let columns = conn
        .browse(&sqlr_core::NodePath(vec![
            "SQLR_TEST".into(),
            "tables".into(),
            customers.clone(),
            "columns".into(),
        ]))
        .await;
    let object = rel
        .introspect()
        .unwrap()
        .load_object(&ObjectRef {
            kind: ObjectKind::Table,
            database: None,
            schema: Some("SQLR_TEST".into()),
            name: orders.clone(),
            parent: None,
            signature: None,
        })
        .await;
    drop_all().await;
    assert_eq!(email.as_deref(), Some("VARCHAR2(120)"));
    let columns = columns.unwrap();
    assert!(
        columns.iter().any(|n| n.name == "EMAIL"
            && n.detail
                .as_deref()
                .is_some_and(|d| d.contains("VARCHAR2(120)"))),
        "{columns:#?}"
    );
    assert!(matches!(object.unwrap(), ObjectDef::Table(t) if t.table.name == orders));
}

#[tokio::test]
async fn upsert_is_a_merge_on_the_server() {
    sqlr_testkit::skip_container_tests!();
    let conn = support::connect().await;
    let rel = conn.relational().unwrap();
    let name = unique_name("SQLR_UPS").to_uppercase();
    rel.execute(&format!(
        "CREATE TABLE {name} (id NUMBER(10) PRIMARY KEY, v VARCHAR2(20))"
    ))
    .await
    .unwrap();
    let merge = OracleDialect
        .upsert_row_sql(
            &format!("\"SQLR_TEST\".\"{name}\""),
            &["ID".into(), "V".into()],
            &[":1".into(), ":2".into()],
            &["ID".into()],
            &["V".into()],
        )
        .unwrap();
    let mut results = Vec::new();
    for v in ["a", "città"] {
        results.push(
            rel.execute_with(&merge, &[Value::Int(1), Value::Text(v.into())])
                .await,
        );
    }
    let rows = rel
        .execute(&format!("SELECT v FROM {name}"))
        .await
        .map(|e| match e {
            Execution::Rows(_) => "rows",
            Execution::Affected(_) => "affected",
        });
    let _ = rel.execute(&format!("DROP TABLE {name} PURGE")).await;
    for r in results {
        assert!(matches!(r.unwrap(), Execution::Affected(1)));
    }
    assert_eq!(rows.unwrap(), "rows");
}
