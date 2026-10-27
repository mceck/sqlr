//! The demo database: a SQLite file in the data directory, generated deterministically
//! (fixed-seed xorshift) the first time "Open demo database" is used, then opened as the
//! saved connection "Demo" with a query on `order_summary`.
//!
//! Everything runs through the engine, on the `Meta` session of a temporary SQLite config:
//! the file is probed by browsing its tables, and generated with one transactional DDL
//! apply (no history entries). A file is regenerated only when it is ours (it has the
//! `sqlr_demo_meta` marker table) or empty; any other `demo.sqlite` is left untouched and the
//! demo goes to `demo-sqlr.sqlite` instead.

use std::path::{Path, PathBuf};

use sqlr_core::{ConnectionConfig, ConnectionProfile, NodePath, Secrets};
use sqlr_engine::{ApplyOptions, JobId, JobOutcome, LoadState, SessionKey, SessionStatus};

use crate::app::SqlrApp;
use crate::store_worker::StoreOp;

/// The saved demo connection's id.
pub(crate) const DEMO_ID: &str = "sqlr-demo";
const DEMO_NAME: &str = "Demo";
/// The query the demo tab opens with.
pub(crate) const DEMO_QUERY: &str = "SELECT * FROM order_summary LIMIT 100;";
const FILE: &str = "demo.sqlite";
/// Where the demo goes when `demo.sqlite` is someone else's.
const ALTERNATE: &str = "demo-sqlr.sqlite";
/// The marker table: a file holding it was written by sqlr.
const MARKER: &str = "sqlr_demo_meta";
const VERSION: u32 = 1;

/// Rows per `INSERT` statement.
const BATCH: usize = 500;

/// A fixed-seed xorshift64: the same data on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number in `0..n`.
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// `YYYY-MM-DD` of a day counted from 1970-01-01.
pub(crate) fn date(days: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn text(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `INSERT INTO table VALUES (…), (…)` in batches.
fn inserts(out: &mut Vec<String>, table: &str, rows: &[String]) {
    for chunk in rows.chunks(BATCH) {
        out.push(format!("INSERT INTO {table} VALUES {}", chunk.join(", ")));
    }
}

const SCHEMA: &[&str] = &[
    "DROP VIEW IF EXISTS order_summary",
    "DROP TABLE IF EXISTS order_items",
    "DROP TABLE IF EXISTS orders",
    "DROP TABLE IF EXISTS products",
    "DROP TABLE IF EXISTS customers",
    "DROP TABLE IF EXISTS employees",
    "DROP TABLE IF EXISTS departments",
    "DROP TABLE IF EXISTS sqlr_demo_meta",
    "CREATE TABLE departments (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL UNIQUE,
    location TEXT NOT NULL,
    budget   REAL NOT NULL DEFAULT 0
)",
    "CREATE TABLE employees (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL,
    email         TEXT    NOT NULL UNIQUE,
    department_id INTEGER REFERENCES departments(id),
    title         TEXT    NOT NULL,
    salary        REAL    NOT NULL,
    hired_at      TEXT    NOT NULL,
    active        INTEGER NOT NULL DEFAULT 1
)",
    "CREATE TABLE customers (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    country    TEXT NOT NULL,
    segment    TEXT NOT NULL,
    created_at TEXT NOT NULL
)",
    "CREATE TABLE products (
    id       INTEGER PRIMARY KEY,
    name     TEXT    NOT NULL,
    category TEXT    NOT NULL,
    price    REAL    NOT NULL,
    stock    INTEGER NOT NULL
)",
    "CREATE TABLE orders (
    id          INTEGER PRIMARY KEY,
    customer_id INTEGER NOT NULL REFERENCES customers(id),
    employee_id INTEGER NOT NULL REFERENCES employees(id),
    status      TEXT    NOT NULL,
    total       REAL    NOT NULL,
    placed_at   TEXT    NOT NULL
)",
    "CREATE TABLE order_items (
    id         INTEGER PRIMARY KEY,
    order_id   INTEGER NOT NULL REFERENCES orders(id),
    product_id INTEGER NOT NULL REFERENCES products(id),
    quantity   INTEGER NOT NULL,
    unit_price REAL    NOT NULL
)",
    "CREATE INDEX idx_orders_customer ON orders(customer_id)",
    "CREATE INDEX idx_orders_placed ON orders(placed_at)",
    "CREATE INDEX idx_employees_department ON employees(department_id)",
    "CREATE INDEX idx_order_items_order ON order_items(order_id)",
    "CREATE VIEW order_summary AS
SELECT o.id, c.name AS customer, e.name AS sales_rep, o.status, o.total, o.placed_at,
       COUNT(i.id) AS items
FROM orders o
JOIN customers c ON c.id = o.customer_id
JOIN employees e ON e.id = o.employee_id
LEFT JOIN order_items i ON i.order_id = o.id
GROUP BY o.id",
    "CREATE TABLE sqlr_demo_meta (version INTEGER NOT NULL)",
];

/// Every statement that (re)creates the demo database, in order; the marker row last.
pub(crate) fn script() -> Vec<String> {
    let mut out: Vec<String> = SCHEMA.iter().map(|s| s.to_string()).collect();
    let mut rng = Rng(0x5157_4C52_2026_0001);

    let departments = [
        ("Engineering", "Berlin", 2_400_000),
        ("Product", "Amsterdam", 1_150_000),
        ("Design", "Lisbon", 780_000),
        ("Sales", "London", 1_900_000),
        ("Support", "Krakow", 620_000),
        ("Finance", "Zurich", 840_000),
    ];
    let rows: Vec<String> = departments
        .iter()
        .enumerate()
        .map(|(i, (name, location, budget))| {
            format!("({}, {}, {}, {budget})", i + 1, text(name), text(location))
        })
        .collect();
    inserts(&mut out, "departments", &rows);

    let first = [
        "Ada",
        "Linus",
        "Grace",
        "Alan",
        "Barbara",
        "Ken",
        "Margaret",
        "Dennis",
        "Radia",
        "Guido",
        "Anita",
        "Bjarne",
        "Katherine",
        "Donald",
        "Frances",
        "Edsger",
    ];
    let last = [
        "Lovelace",
        "Torvalds",
        "Hopper",
        "Turing",
        "Liskov",
        "Thompson",
        "Hamilton",
        "Ritchie",
        "Perlman",
        "Rossum",
        "Borg",
        "Stroustrup",
        "Johnson",
        "Knuth",
    ];
    let titles = [
        "Software Engineer",
        "Senior Software Engineer",
        "Staff Engineer",
        "Product Manager",
        "Designer",
        "Account Executive",
        "Support Engineer",
        "Analyst",
    ];
    let rows: Vec<String> = (1..=40)
        .map(|id| {
            let (f, l) = (rng.pick(&first), rng.pick(&last));
            let department = 1 + rng.below(departments.len());
            let salary = 52_000 + rng.below(90) * 1_100;
            let hired = date(17_500 + rng.below(3_000) as i64);
            let email = format!("{}.{}.{id}@example.com", f.to_lowercase(), l.to_lowercase());
            let active = i32::from(id % 9 != 0);
            format!(
                "({id}, {}, {}, {department}, {}, {salary}, {}, {active})",
                text(&format!("{f} {l}")),
                text(&email),
                text(rng.pick(&titles)),
                text(&hired),
            )
        })
        .collect();
    inserts(&mut out, "employees", &rows);

    let companies = [
        "Northwind",
        "Acme",
        "Globex",
        "Initech",
        "Umbrella",
        "Hooli",
        "Vandelay",
        "Soylent",
        "Stark",
        "Wayne",
        "Cyberdyne",
        "Wonka",
        "Tyrell",
        "Aperture",
        "Monarch",
    ];
    let suffixes = [
        "Labs",
        "Systems",
        "Trading",
        "Industries",
        "Group",
        "Digital",
        "Works",
        "Partners",
    ];
    let countries = [
        "Germany",
        "Netherlands",
        "Portugal",
        "United Kingdom",
        "Poland",
        "Switzerland",
        "France",
        "Spain",
        "Italy",
        "Sweden",
    ];
    let segments = ["Enterprise", "Mid-market", "SMB", "Startup"];
    let rows: Vec<String> = (1..=200)
        .map(|id| {
            let name = format!("{} {}", rng.pick(&companies), rng.pick(&suffixes));
            let created = date(17_000 + rng.below(3_500) as i64);
            format!(
                "({id}, {}, {}, {}, {})",
                text(&name),
                text(rng.pick(&countries)),
                text(rng.pick(&segments)),
                text(&created)
            )
        })
        .collect();
    inserts(&mut out, "customers", &rows);

    let products = [
        "Compute",
        "Storage",
        "Database",
        "CDN",
        "Analytics",
        "Queue",
        "Vault",
        "Gateway",
        "Runner",
        "Registry",
        "Pipeline",
        "Tracer",
        "Alerts",
        "Flags",
        "Scheduler",
    ];
    let tiers = ["Basic", "Pro", "Team", "Enterprise"];
    let categories = [
        "Cloud",
        "Data",
        "Security",
        "Developer Tools",
        "Observability",
        "Network",
    ];
    let mut prices = Vec::with_capacity(60);
    let rows: Vec<String> = (1..=60)
        .map(|id| {
            let name = format!(
                "{} {}",
                products[(id - 1) % products.len()],
                tiers[(id - 1) / products.len()]
            );
            let price = 9 + rng.below(490);
            prices.push(price);
            let stock = rng.below(1_000);
            format!(
                "({id}, {}, {}, {price}, {stock})",
                text(&name),
                text(rng.pick(&categories))
            )
        })
        .collect();
    inserts(&mut out, "products", &rows);

    let statuses = [
        "delivered",
        "delivered",
        "delivered",
        "shipped",
        "processing",
        "cancelled",
    ];
    let mut orders = Vec::with_capacity(2_000);
    let mut items = Vec::with_capacity(6_000);
    for id in 1..=2_000usize {
        let customer = 1 + rng.below(200);
        let employee = 1 + rng.below(40);
        let placed = date(19_000 + rng.below(1_400) as i64);
        let status = rng.pick(&statuses);
        let mut total = 0;
        for _ in 0..1 + rng.below(5) {
            let product = 1 + rng.below(60);
            let quantity = 1 + rng.below(4);
            let price = prices[product - 1];
            total += price * quantity;
            items.push(format!(
                "({}, {id}, {product}, {quantity}, {price})",
                items.len() + 1
            ));
        }
        orders.push(format!(
            "({id}, {customer}, {employee}, {}, {total}, {})",
            text(status),
            text(&placed)
        ));
    }
    inserts(&mut out, "orders", &orders);
    inserts(&mut out, "order_items", &items);
    out.push(format!("INSERT INTO {MARKER} VALUES ({VERSION})"));
    out
}

/// Where the demo setup is.
enum Stage {
    /// The temporary session connects to the file.
    Connecting,
    /// Its tables and views are being listed.
    Probing,
    /// The script runs.
    Generating(JobId),
    /// The "Demo" connection is being saved.
    Saving,
}

pub(crate) struct DemoSetup {
    /// The demo file being probed or generated.
    path: PathBuf,
    /// `path` is the alternate file (the usual one was someone else's).
    alternate: bool,
    /// The temporary connection's id (transient: it holds a `:`).
    temp: String,
    stage: Stage,
}

/// What the file holds, from its table and view names.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// Our demo, complete.
    Ready,
    /// Ours but incomplete, or empty: generate it.
    Generate,
    /// Someone else's file.
    Foreign,
}

fn probe(tables: &[String], views: &[String]) -> Probe {
    let has = |list: &[String], name: &str| list.iter().any(|t| t == name);
    if has(tables, MARKER) {
        if has(tables, "orders") && has(views, "order_summary") {
            Probe::Ready
        } else {
            Probe::Generate
        }
    } else if tables.is_empty() && views.is_empty() {
        Probe::Generate
    } else {
        Probe::Foreign
    }
}

fn temp_config(id: &str, path: &Path) -> ConnectionConfig {
    ConnectionConfig {
        id: id.to_string(),
        name: "Demo setup".into(),
        driver: "sqlite".into(),
        group: None,
        color: None,
        values: [
            ("path".to_string(), path.to_string_lossy().into_owned()),
            ("create_if_missing".to_string(), "true".to_string()),
        ]
        .into(),
        profile: None,
        save_secrets: true,
        agent_access: Default::default(),
    }
}

fn names(
    state: Option<LoadState<Vec<sqlr_core::Node>>>,
) -> Option<Result<Vec<String>, sqlr_core::DriverError>> {
    match state? {
        LoadState::Ready(nodes) => Some(Ok(nodes.into_iter().map(|n| n.name).collect())),
        LoadState::Failed(e) => Some(Err(e)),
        LoadState::Loading => None,
    }
}

impl SqlrApp {
    /// "Open demo database": prepares the file (in the background), then opens it.
    pub(crate) fn open_demo(&mut self) {
        if self.demo.is_some() {
            return;
        }
        if self.manifest("sqlite").is_none() {
            self.notify("Could not open the demo database: the SQLite driver is missing");
            return;
        }
        let path = self.data_dir.join(FILE);
        self.demo = Some(self.start_probe(path, false));
    }

    fn start_probe(&self, path: PathBuf, alternate: bool) -> DemoSetup {
        let temp = format!("demo-setup:{}", uuid::Uuid::new_v4());
        self.engine.connect(
            temp_config(&temp, &path),
            Secrets::default(),
            ConnectionProfile::development(),
        );
        DemoSetup {
            path,
            alternate,
            temp,
            stage: Stage::Connecting,
        }
    }

    /// Follows the demo setup (every frame).
    pub(crate) fn demo_ui(&mut self) {
        let Some(setup) = self.demo.take() else {
            return;
        };
        self.demo = self.advance_demo(setup);
    }

    fn advance_demo(&mut self, mut setup: DemoSetup) -> Option<DemoSetup> {
        let main = NodePath::root().child("main");
        match setup.stage {
            Stage::Connecting => match self.engine.session_status(&setup.temp) {
                Some(SessionStatus::Connected(_)) => {
                    self.engine.browse(&setup.temp, main.child("tables"));
                    self.engine.browse(&setup.temp, main.child("views"));
                    setup.stage = Stage::Probing;
                    Some(setup)
                }
                Some(SessionStatus::Failed(e)) => self.demo_failed(&setup, &e.message),
                _ => Some(setup),
            },
            Stage::Probing => {
                let tables = names(self.engine.children(&setup.temp, &main.child("tables")));
                let views = names(self.engine.children(&setup.temp, &main.child("views")));
                let (Some(tables), Some(views)) = (tables, views) else {
                    return Some(setup);
                };
                let (tables, views) = match (tables, views) {
                    (Ok(t), Ok(v)) => (t, v),
                    (Err(e), _) | (_, Err(e)) => return self.demo_failed(&setup, &e.message),
                };
                match probe(&tables, &views) {
                    Probe::Ready => Some(self.demo_ready(setup)),
                    Probe::Generate => {
                        let options = ApplyOptions {
                            transactional: true,
                            ..Default::default()
                        };
                        let job = self.engine.apply_ddl(
                            &SessionKey::meta(&setup.temp),
                            script(),
                            options,
                        );
                        setup.stage = Stage::Generating(job);
                        Some(setup)
                    }
                    Probe::Foreign if !setup.alternate => {
                        // Someone else's demo.sqlite: never touched; ours goes next to it.
                        self.engine.disconnect(&setup.temp);
                        let path = self.data_dir.join(ALTERNATE);
                        Some(self.start_probe(path, true))
                    }
                    Probe::Foreign => self.demo_failed(
                        &setup,
                        &format!("{FILE} and {ALTERNATE} hold other data; neither was changed"),
                    ),
                }
            }
            Stage::Generating(job) => {
                let state = self.engine.job(job);
                if state.as_ref().is_some_and(|s| s.outcome.is_none()) {
                    return Some(setup);
                }
                self.engine.forget_job(job);
                match state.and_then(|s| s.outcome) {
                    Some(JobOutcome::DdlApplied(report)) if report.ok() => {
                        Some(self.demo_ready(setup))
                    }
                    Some(JobOutcome::DdlApplied(report)) => {
                        self.demo_failed(&setup, &report.summary())
                    }
                    Some(JobOutcome::Failed(e)) => self.demo_failed(&setup, &e.message),
                    _ => self.demo_failed(&setup, "the database was not written"),
                }
            }
            Stage::Saving => {
                let path = setup.path.to_string_lossy();
                let saved = self
                    .connection(DEMO_ID)
                    .is_some_and(|c| c.values.get("path").map(String::as_str) == Some(&*path));
                if !saved {
                    return Some(setup);
                }
                self.open_and_run(DEMO_NAME.to_string(), DEMO_ID, None, DEMO_QUERY);
                None
            }
        }
    }

    /// The file is ready: the temporary session ends and the "Demo" connection opens it
    /// (saved first, or re-pointed, when needed).
    fn demo_ready(&mut self, mut setup: DemoSetup) -> DemoSetup {
        self.engine.disconnect(&setup.temp);
        let path = setup.path.to_string_lossy().into_owned();
        let existing = self.connection(DEMO_ID).cloned();
        let current = existing
            .as_ref()
            .is_some_and(|c| c.values.get("path") == Some(&path));
        if !current {
            let mut cfg = existing.clone().unwrap_or_else(|| ConnectionConfig {
                id: DEMO_ID.into(),
                name: DEMO_NAME.into(),
                driver: "sqlite".into(),
                group: None,
                color: None,
                values: Default::default(),
                profile: None,
                save_secrets: true,
                agent_access: Default::default(),
            });
            cfg.values.insert("path".into(), path);
            cfg.values
                .insert("create_if_missing".into(), "false".into());
            self.store.send(StoreOp::Save {
                cfg,
                secrets: Secrets::default(),
                editing: existing.is_some(),
            });
        }
        setup.stage = Stage::Saving;
        setup
    }

    fn demo_failed(&mut self, setup: &DemoSetup, message: &str) -> Option<DemoSetup> {
        self.engine.disconnect(&setup.temp);
        tracing::warn!(path = %setup.path.display(), error = message, "demo database setup failed");
        self.notify(format!("Could not open the demo database: {message}"));
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(script: &[String], table: &str) -> usize {
        let prefix = format!("INSERT INTO {table} VALUES ");
        script
            .iter()
            .filter_map(|s| s.strip_prefix(&prefix))
            .map(|values| values.matches("), (").count() + 1)
            .sum()
    }

    #[test]
    fn script_is_deterministic_with_the_planned_sizes() {
        let script = script();
        assert_eq!(script, super::script(), "the same data every time");
        assert_eq!(rows(&script, "departments"), 6);
        assert_eq!(rows(&script, "employees"), 40);
        assert_eq!(rows(&script, "customers"), 200);
        assert_eq!(rows(&script, "products"), 60);
        assert_eq!(rows(&script, "orders"), 2_000);
        let items = rows(&script, "order_items");
        assert!((5_000..=7_000).contains(&items), "{items}");
        // The marker comes last: a failed run never leaves a file that looks complete.
        assert_eq!(
            script.last().unwrap(),
            "INSERT INTO sqlr_demo_meta VALUES (1)"
        );
    }

    #[test]
    fn probe_regenerates_only_our_files() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let ours = s(&[MARKER, "orders", "customers"]);
        assert_eq!(probe(&ours, &s(&["order_summary"])), Probe::Ready);
        assert_eq!(probe(&s(&[MARKER]), &[]), Probe::Generate);
        assert_eq!(probe(&[], &[]), Probe::Generate);
        assert_eq!(
            probe(&s(&["orders"]), &s(&["order_summary"])),
            Probe::Foreign
        );
        assert_eq!(probe(&[], &s(&["v"])), Probe::Foreign);
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(19_723), "2024-01-01");
        assert_eq!(date(19_782), "2024-02-29");
    }
}
