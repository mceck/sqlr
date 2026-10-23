//! Query plans (spec §8.6): the plan tree model and the parsers of each server's explain
//! output. Parsing is pure text in, tree out; the drivers run the statements.

use serde_json::Value as Json;

use crate::error::{DriverError, ErrorKind};
use crate::syntax::{LexState, SqlSyntax};
use crate::value::Value;

/// How a server's explain output becomes a [`PlanTree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplainParser {
    PgJson,
    MysqlTree,
    MysqlJson,
    SqliteRows,
    /// The lines of `DBMS_XPLAN.DISPLAY`.
    OracleText,
    /// The rows of `SET SHOWPLAN_ALL ON`.
    MssqlShowplan,
}

impl ExplainParser {
    /// Parses the rows the explain statement returned.
    pub fn parse(self, rows: &[Vec<Value>]) -> Result<PlanTree, DriverError> {
        let text = |r: &Vec<Value>| {
            r.first()
                .map(|v| v.display().into_owned())
                .unwrap_or_default()
        };
        match self {
            ExplainParser::PgJson => {
                parse_pg_json(&rows.iter().map(text).collect::<Vec<_>>().join("\n"))
            }
            ExplainParser::MysqlTree => {
                parse_mysql_tree(&rows.iter().map(text).collect::<Vec<_>>().join("\n"))
            }
            ExplainParser::MysqlJson => {
                parse_mysql_json(&rows.iter().map(text).collect::<Vec<_>>().join("\n"))
            }
            ExplainParser::OracleText => {
                parse_oracle_text(&rows.iter().map(text).collect::<Vec<_>>().join("\n"))
            }
            ExplainParser::MssqlShowplan => parse_mssql_showplan(rows),
            ExplainParser::SqliteRows => {
                let int = |v: Option<&Value>| match v {
                    Some(Value::Int(i)) => *i,
                    _ => 0,
                };
                let parsed: Vec<(i64, i64, String)> = rows
                    .iter()
                    .map(|r| {
                        (
                            int(r.first()),
                            int(r.get(1)),
                            r.get(3)
                                .map(|v| v.display().into_owned())
                                .unwrap_or_default(),
                        )
                    })
                    .collect();
                parse_sqlite_rows(&parsed)
            }
        }
    }
}

/// The statements that explain one statement: run `before`, then `sql` (its rows are the
/// plan), then `after` (always, even when the plan failed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplainSpec {
    pub before: Vec<String>,
    pub sql: String,
    pub after: Vec<String>,
    pub parser: ExplainParser,
}

/// `sql` as the one statement to explain, or why it cannot be: several statements, DDL and
/// anything but a query or a data change have no plan to show.
pub fn explain_check(syntax: &SqlSyntax, sql: &str) -> Result<String, DriverError> {
    let ranges = syntax.split(sql, LexState::default());
    let unsupported = |m: &str| DriverError::new(ErrorKind::Unsupported, m);
    let [range] = ranges.as_slice() else {
        return Err(unsupported(
            "Explain works on one statement at a time: put the caret in the statement to explain",
        ));
    };
    let statement = sql[range.clone()].trim().to_string();
    let first = statement
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if !matches!(
        first.as_str(),
        "SELECT"
            | "WITH"
            | "VALUES"
            | "TABLE"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "REPLACE"
            | "MERGE"
    ) {
        return Err(unsupported(
            "Explain shows the plan of a query or a data change, not of this kind of statement",
        ));
    }
    Ok(statement)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanNode {
    pub label: String,
    pub kind: String,
    pub relation: Option<String>,
    /// `(startup, total)` estimated cost.
    pub cost: Option<(f64, f64)>,
    pub rows_estimate: Option<f64>,
    pub rows_actual: Option<f64>,
    pub loops: Option<f64>,
    /// Inclusive time over all loops, in milliseconds.
    pub time_ms: Option<f64>,
    /// Time spent in the node itself.
    pub self_time_ms: Option<f64>,
    pub buffers: Vec<(String, i64)>,
    pub extra: Vec<(String, String)>,
    pub children: Vec<PlanNode>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanTree {
    pub root: PlanNode,
    pub planning_ms: Option<f64>,
    pub execution_ms: Option<f64>,
    /// The plan as the server printed it.
    pub raw: String,
    /// `json`, `tree` or `text`.
    pub format: String,
}

/// What an operator does, for the summary counts and the card colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanRole {
    Join,
    Scan,
    Sort,
    Aggregate,
    Other,
}

/// How serious a [`PlanIssue`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlanIssueLevel {
    /// Worth knowing, not a problem (the plan carries no measured times).
    Note,
    Warning,
}

/// One thing the summary says about a plan node in plain words.
#[derive(Clone, Debug, PartialEq)]
pub struct PlanIssue {
    pub level: PlanIssueLevel,
    /// The node the issue is about, as child indexes from the root (`[]` is the root).
    pub path: Vec<usize>,
    /// The node as "Seq Scan on orders"; empty for a plan-wide note.
    pub node: String,
    pub message: String,
}

/// The composition of a plan and its issues: what the summary shows above the tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlanInsights {
    pub nodes: usize,
    /// The deepest node, the root counting as 1.
    pub depth: usize,
    pub joins: usize,
    pub scans: usize,
    pub sorts: usize,
    pub aggregates: usize,
    /// Whether the plan carries measured times (`EXPLAIN ANALYZE`).
    pub timed: bool,
    /// Warnings first, then plan-wide notes.
    pub issues: Vec<PlanIssue>,
}

impl PlanNode {
    /// The node as one line: "Seq Scan on orders" (the relation is added when the label
    /// does not already name it).
    pub fn title(&self) -> String {
        match &self.relation {
            Some(relation) if !self.label.contains(relation.as_str()) => {
                format!("{} on {relation}", self.label)
            }
            _ => self.label.clone(),
        }
    }

    /// What the operator does, from the kinds and labels the parsers produce.
    pub fn role(&self) -> PlanRole {
        let text = format!("{} {}", self.kind, self.label).to_ascii_lowercase();
        if text.contains("join") || text.contains("nested loop") {
            return PlanRole::Join;
        }
        if text.contains("scan")
            || text.contains("search")
            || text.contains("table access")
            || text.contains("index lookup")
            // MySQL's JSON plan names the access method instead of a "scan".
            || matches!(
                self.kind.to_ascii_lowercase().as_str(),
                "all" | "ref" | "eq_ref" | "range" | "index" | "index_merge" | "fulltext"
            )
        {
            return PlanRole::Scan;
        }
        if text.contains("sort") || text.contains("order by") || text.contains("ordering operation")
        {
            return PlanRole::Sort;
        }
        if text.contains("aggregate")
            || text.contains("group by")
            || text.contains("grouping operation")
            || text.contains("windowagg")
        {
            return PlanRole::Aggregate;
        }
        PlanRole::Other
    }

    /// What makes the node expensive: its own time when timed, else its own cost.
    pub fn weight(&self) -> f64 {
        if let Some(t) = self.self_time_ms {
            return t;
        }
        let total = self.cost.map_or(0.0, |c| c.1);
        let below: f64 = self
            .children
            .iter()
            .filter_map(|c| c.cost.map(|c| c.1))
            .sum();
        (total - below).max(0.0)
    }

    fn fill_self_times(&mut self) {
        for c in &mut self.children {
            c.fill_self_times();
        }
        if let Some(total) = self.time_ms {
            let below: f64 = self.children.iter().filter_map(|c| c.time_ms).sum();
            self.self_time_ms = Some((total - below).max(0.0));
        }
    }
}

impl PlanTree {
    /// The paths (child indexes from the root) of the `n` heaviest nodes, heaviest first.
    pub fn hottest(&self, n: usize) -> Vec<Vec<usize>> {
        fn walk(node: &PlanNode, path: &mut Vec<usize>, out: &mut Vec<(f64, Vec<usize>)>) {
            out.push((node.weight(), path.clone()));
            for (i, c) in node.children.iter().enumerate() {
                path.push(i);
                walk(c, path, out);
                path.pop();
            }
        }
        let mut all = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut all);
        all.retain(|(w, _)| *w > 0.0);
        all.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        all.into_iter().take(n).map(|(_, p)| p).collect()
    }

    pub fn node(&self, path: &[usize]) -> Option<&PlanNode> {
        path.iter().try_fold(&self.root, |n, &i| n.children.get(i))
    }

    /// The counts, the depth and the issues the summary shows. Every number comes from the
    /// plan itself; the warnings use thresholds that hold across the supported engines.
    pub fn insights(&self) -> PlanInsights {
        /// Measured rows ≥ this many times the estimate (or the other way round).
        const ESTIMATE_FACTOR: f64 = 10.0;
        /// A full scan on a relation this large is worth reporting.
        const FULL_SCAN_ROWS: f64 = 10_000.0;
        /// A node run this often hints at a nested loop over many rows.
        const MANY_LOOPS: f64 = 1_000.0;

        fn walk(node: &PlanNode, path: &mut Vec<usize>, depth: usize, out: &mut PlanInsights) {
            out.nodes += 1;
            out.depth = out.depth.max(depth);
            match node.role() {
                PlanRole::Join => out.joins += 1,
                PlanRole::Scan => out.scans += 1,
                PlanRole::Sort => out.sorts += 1,
                PlanRole::Aggregate => out.aggregates += 1,
                PlanRole::Other => {}
            }
            let title = node.title();
            let warn = |message: String| PlanIssue {
                level: PlanIssueLevel::Warning,
                path: path.clone(),
                node: title.clone(),
                message,
            };
            let loops = node.loops.unwrap_or(1.0).max(1.0);
            let measured_rows = node.rows_actual.map(|rows| rows * loops);
            let scanned = measured_rows.or(node.rows_estimate);

            // The estimate was far off: the planner may have picked the wrong join order.
            // Both counts are per loop, as the plan reports them.
            if let (Some(estimate), Some(actual)) = (node.rows_estimate, node.rows_actual) {
                let (estimate, actual) = (estimate.max(1.0), actual.max(1.0));
                let (more, ratio) = if actual >= estimate {
                    (true, actual / estimate)
                } else {
                    (false, estimate / actual)
                };
                if ratio >= ESTIMATE_FACTOR {
                    out.issues.push(warn(format!(
                        "{}: {} rows estimated, {} read{} ({}× {})",
                        title,
                        grouped(estimate),
                        grouped(actual),
                        if loops > 1.0 { " per loop" } else { "" },
                        grouped(ratio),
                        if more { "more" } else { "fewer" }
                    )));
                }
            }

            // A full scan over a large relation.
            let full = {
                let label = node.label.to_ascii_lowercase();
                let kind = node.kind.to_ascii_lowercase();
                label.contains("seq scan")
                    || label.contains("table scan")
                    || label.contains("table access full")
                    || label.contains("full scan")
                    || kind == "scan"
                    || kind == "all"
            };
            if full && scanned.is_some_and(|rows| rows >= FULL_SCAN_ROWS) {
                let rows = scanned.unwrap_or_default();
                let verb = if measured_rows.is_some() {
                    "reads"
                } else {
                    "is expected to read"
                };
                out.issues.push(warn(format!(
                    "{} {verb} {} rows (full scan)",
                    title,
                    grouped(rows)
                )));
            }

            // The inner side of a nested loop run thousands of times.
            if let Some(loops) = node.loops
                && loops >= MANY_LOOPS
            {
                out.issues
                    .push(warn(format!("{} runs {} times", title, grouped(loops))));
            }

            // Work spilled out of memory.
            let temp: i64 = node
                .buffers
                .iter()
                .filter(|(name, _)| name.starts_with("Temp "))
                .map(|(_, value)| *value)
                .sum();
            if temp > 0 {
                out.issues.push(warn(format!(
                    "{} spills to disk ({} temp blocks)",
                    title,
                    grouped(temp as f64)
                )));
            } else if let Some((_, method)) = node
                .extra
                .iter()
                .find(|(name, _)| name == "Sort Method")
                .filter(|(_, value)| value.to_ascii_lowercase().contains("external"))
            {
                out.issues
                    .push(warn(format!("{} spills to disk ({method})", title)));
            } else if node
                .extra
                .iter()
                .any(|(name, value)| name == "Using temporary" && value == "true")
            {
                out.issues
                    .push(warn(format!("{} uses a temporary table", title)));
            } else if node
                .extra
                .iter()
                .any(|(name, value)| name == "Using filesort" && value == "true")
            {
                out.issues
                    .push(warn(format!("{} sorts with filesort", title)));
            }

            for (i, child) in node.children.iter().enumerate() {
                path.push(i);
                walk(child, path, depth + 1, out);
                path.pop();
            }
        }

        fn grouped(value: f64) -> String {
            let value = value.round() as i64;
            let digits = value.unsigned_abs().to_string();
            let mut out = String::new();
            for (i, c) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i).is_multiple_of(3) {
                    out.push(',');
                }
                out.push(c);
            }
            if value < 0 { format!("-{out}") } else { out }
        }

        let mut out = PlanInsights::default();
        walk(&self.root, &mut Vec::new(), 1, &mut out);
        fn timed(node: &PlanNode) -> bool {
            node.time_ms.is_some() || node.self_time_ms.is_some() || node.children.iter().any(timed)
        }
        out.timed = self.execution_ms.is_some() || timed(&self.root);
        if !out.timed {
            out.issues.push(PlanIssue {
                level: PlanIssueLevel::Note,
                path: Vec::new(),
                node: String::new(),
                message: "Estimated plan: row counts and costs are estimates, not measured times"
                    .into(),
            });
        }
        out.issues
            .sort_by(|a, b| b.level.cmp(&a.level).then_with(|| a.path.cmp(&b.path)));
        out
    }
}

fn bad(message: impl Into<String>) -> DriverError {
    DriverError::new(ErrorKind::Other, message)
}

fn num(v: &Json, key: &str) -> Option<f64> {
    match v.get(key)? {
        Json::Number(n) => n.as_f64(),
        Json::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// PostgreSQL `EXPLAIN (FORMAT JSON)`, with or without `ANALYZE`.
pub fn parse_pg_json(text: &str) -> Result<PlanTree, DriverError> {
    let json: Json = serde_json::from_str(text).map_err(|e| bad(format!("plan: {e}")))?;
    let top = json.get(0).unwrap_or(&json);
    let plan = top.get("Plan").ok_or_else(|| bad("the plan has no root"))?;
    fn node(v: &Json) -> PlanNode {
        const BUFFERS: [&str; 6] = [
            "Shared Hit Blocks",
            "Shared Read Blocks",
            "Shared Dirtied Blocks",
            "Local Hit Blocks",
            "Temp Read Blocks",
            "Temp Written Blocks",
        ];
        let loops = num(v, "Actual Loops");
        let mut n = PlanNode {
            label: v["Node Type"].as_str().unwrap_or("?").to_string(),
            kind: v["Node Type"].as_str().unwrap_or("?").to_string(),
            relation: v["Relation Name"].as_str().map(String::from),
            cost: num(v, "Startup Cost").zip(num(v, "Total Cost")),
            rows_estimate: num(v, "Plan Rows"),
            rows_actual: num(v, "Actual Rows"),
            loops,
            // `Actual Total Time` is per loop.
            time_ms: num(v, "Actual Total Time").map(|t| t * loops.unwrap_or(1.0)),
            ..PlanNode::default()
        };
        for b in BUFFERS {
            if let Some(x) = v.get(b).and_then(Json::as_i64) {
                n.buffers.push((b.to_string(), x));
            }
        }
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                let known = matches!(
                    k.as_str(),
                    "Node Type"
                        | "Relation Name"
                        | "Plans"
                        | "Startup Cost"
                        | "Total Cost"
                        | "Plan Rows"
                        | "Plan Width"
                        | "Actual Rows"
                        | "Actual Loops"
                        | "Actual Startup Time"
                        | "Actual Total Time"
                        | "Parallel Aware"
                        | "Async Capable"
                ) || BUFFERS.contains(&k.as_str());
                if known {
                    continue;
                }
                match val {
                    Json::String(s) => n.extra.push((k.clone(), s.clone())),
                    Json::Number(x) => n.extra.push((k.clone(), x.to_string())),
                    Json::Bool(b) => n.extra.push((k.clone(), b.to_string())),
                    _ => {}
                }
            }
        }
        n.children = v["Plans"]
            .as_array()
            .map(|a| a.iter().map(node).collect())
            .unwrap_or_default();
        n
    }
    let mut root = node(plan);
    root.fill_self_times();
    Ok(PlanTree {
        root,
        planning_ms: num(top, "Planning Time"),
        execution_ms: num(top, "Execution Time"),
        raw: text.to_string(),
        format: "json".into(),
    })
}

/// MySQL 8 `EXPLAIN FORMAT=TREE` / `EXPLAIN ANALYZE`: one `-> node` per line, nested by
/// indentation.
pub fn parse_mysql_tree(text: &str) -> Result<PlanTree, DriverError> {
    fn numbers(line: &str, key: &str) -> Option<f64> {
        let at = line.find(key)? + key.len();
        let rest = &line[at..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'e' || c == '-'))
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    }
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots: Vec<PlanNode> = Vec::new();
    for line in text.lines() {
        let Some(arrow) = line.find("-> ") else {
            continue;
        };
        let body = &line[arrow + 3..];
        let depth = arrow;
        // "Label  (cost=1.2 rows=3) (actual time=0.1..0.4 rows=3 loops=2)"
        let label = body.split("  (").next().unwrap_or(body).trim().to_string();
        let cost = numbers(body, "cost=").map(|c| (0.0, c));
        let loops = numbers(body, "loops=");
        let mut n = PlanNode {
            kind: label.split([':', ' ']).next().unwrap_or("").to_string(),
            relation: label
                .split(" on ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .map(String::from),
            label,
            cost,
            rows_estimate: numbers(body, "rows="),
            loops,
            ..PlanNode::default()
        };
        if let Some(at) = body.find("actual time=") {
            let part = &body[at..];
            n.time_ms = part
                .split("..")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|t| t.parse::<f64>().ok())
                .map(|t| t * loops.unwrap_or(1.0));
            n.rows_actual = numbers(part, "rows=");
        }
        while stack.last().is_some_and(|(d, _)| *d >= depth) {
            let (_, done) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        stack.push((depth, n));
    }
    while let Some((_, done)) = stack.pop() {
        match stack.last_mut() {
            Some((_, parent)) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    let mut root = match roots.len() {
        0 => return Err(bad("the plan is empty")),
        1 => roots.remove(0),
        _ => PlanNode {
            label: "Plan".into(),
            kind: "Plan".into(),
            children: roots,
            ..PlanNode::default()
        },
    };
    root.fill_self_times();
    Ok(PlanTree {
        execution_ms: root.time_ms,
        root,
        planning_ms: None,
        raw: text.to_string(),
        format: "tree".into(),
    })
}

/// MySQL and MariaDB `EXPLAIN FORMAT=JSON` (and MariaDB's `ANALYZE FORMAT=JSON`).
pub fn parse_mysql_json(text: &str) -> Result<PlanTree, DriverError> {
    let json: Json = serde_json::from_str(text).map_err(|e| bad(format!("plan: {e}")))?;
    let block = json
        .get("query_block")
        .ok_or_else(|| bad("the plan has no query block"))?;
    fn table(v: &Json) -> PlanNode {
        let access = v["access_type"].as_str().unwrap_or("table");
        let name = v["table_name"].as_str().unwrap_or("?");
        let cost = v["cost_info"].get("prefix_cost").and_then(|c| match c {
            Json::String(s) => s.parse::<f64>().ok(),
            Json::Number(n) => n.as_f64(),
            _ => None,
        });
        let mut n = PlanNode {
            label: format!("{access} on {name}"),
            kind: access.to_string(),
            relation: Some(name.to_string()),
            cost: cost.map(|c| (0.0, c)),
            rows_estimate: num(v, "rows_examined_per_scan").or_else(|| num(v, "rows")),
            rows_actual: num(v, "r_rows"),
            loops: num(v, "r_loops"),
            time_ms: num(v, "r_total_time_ms"),
            ..PlanNode::default()
        };
        if let Some(key) = v["key"].as_str() {
            n.extra.push(("key".into(), key.to_string()));
        }
        if let Some(c) = v["attached_condition"].as_str() {
            n.extra.push(("condition".into(), c.to_string()));
        }
        n
    }
    fn walk(label: &str, v: &Json) -> PlanNode {
        let mut n = PlanNode {
            label: label.to_string(),
            kind: label.to_string(),
            cost: v["cost_info"]
                .get("query_cost")
                .and_then(|c| {
                    c.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| c.as_f64())
                })
                .map(|c| (0.0, c)),
            ..PlanNode::default()
        };
        let Some(obj) = v.as_object() else {
            return n;
        };
        for (k, val) in obj {
            match (k.as_str(), val) {
                ("table", t) if t.is_object() => n.children.push(table(t)),
                ("nested_loop", Json::Array(items)) => {
                    for item in items {
                        if let Some(t) = item.get("table") {
                            n.children.push(table(t));
                        }
                    }
                }
                (
                    "ordering_operation"
                    | "grouping_operation"
                    | "duplicates_removal"
                    | "buffer_result"
                    | "materialized_from_subquery"
                    | "union_result",
                    inner,
                ) if inner.is_object() => n.children.push(walk(&k.replace('_', " "), inner)),
                _ => {}
            }
        }
        n
    }
    let mut root = walk("Query block", block);
    root.fill_self_times();
    Ok(PlanTree {
        root,
        planning_ms: None,
        execution_ms: block.get("r_total_time_ms").and_then(Json::as_f64),
        raw: text.to_string(),
        format: "json".into(),
    })
}

/// A count as `DBMS_XPLAN` prints it: `14`, `1203`, `12K`, `3M`.
fn xplan_number(text: &str) -> Option<f64> {
    let text = text.trim();
    let (digits, scale) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 1e3),
        'M' => (&text[..text.len() - 1], 1e6),
        'G' => (&text[..text.len() - 1], 1e9),
        'T' => (&text[..text.len() - 1], 1e12),
        _ => (text, 1.0),
    };
    digits.trim().parse::<f64>().ok().map(|n| n * scale)
}

/// The plan table of Oracle's `DBMS_XPLAN.DISPLAY`: one row per operation, its depth the
/// indentation of the `Operation` cell.
pub fn parse_oracle_text(text: &str) -> Result<PlanTree, DriverError> {
    let mut columns: Vec<String> = Vec::new();
    let mut stack: Vec<(usize, PlanNode)> = Vec::new();
    let mut roots: Vec<PlanNode> = Vec::new();
    for line in text.lines() {
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_end().trim_matches('|').split('|').collect();
        if columns.is_empty() {
            if cells.iter().any(|c| c.trim() == "Operation") {
                columns = cells.iter().map(|c| c.trim().to_string()).collect();
            }
            continue;
        }
        let cell = |name: &str| {
            columns
                .iter()
                .position(|c| c == name)
                .and_then(|i| cells.get(i))
        };
        let Some(operation) = cell("Operation") else {
            continue;
        };
        let depth = operation.len() - operation.trim_start().len();
        let operation = operation.trim().to_string();
        let name = cell("Name")
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty());
        let mut node = PlanNode {
            label: match &name {
                Some(name) => format!("{operation} {name}"),
                None => operation.clone(),
            },
            kind: operation,
            relation: name,
            rows_estimate: cell("Rows").and_then(|c| xplan_number(c)),
            ..PlanNode::default()
        };
        if let Some(cost) = cell("Cost (%CPU)")
            .and_then(|c| c.split_whitespace().next())
            .and_then(xplan_number)
        {
            node.cost = Some((0.0, cost));
        }
        while stack.last().is_some_and(|(d, _)| *d >= depth) {
            let (_, done) = stack.pop().unwrap();
            match stack.last_mut() {
                Some((_, parent)) => parent.children.push(done),
                None => roots.push(done),
            }
        }
        stack.push((depth, node));
    }
    while let Some((_, done)) = stack.pop() {
        match stack.last_mut() {
            Some((_, parent)) => parent.children.push(done),
            None => roots.push(done),
        }
    }
    let root = match roots.len() {
        0 => return Err(bad("the plan is empty")),
        1 => roots.remove(0),
        _ => PlanNode {
            label: "Plan".into(),
            kind: "Plan".into(),
            children: roots,
            ..PlanNode::default()
        },
    };
    Ok(PlanTree {
        root,
        planning_ms: None,
        execution_ms: None,
        raw: text.to_string(),
        format: "text".into(),
    })
}

/// SQL Server's `SET SHOWPLAN_ALL ON` rows (`StmtText`, `StmtId`, `NodeId`, `Parent`,
/// `PhysicalOp`, `LogicalOp`, `Argument`, `DefinedValues`, `EstimateRows`, `EstimateIO`,
/// `EstimateCPU`, `AvgRowSize`, `TotalSubtreeCost`, …): the tree is `NodeId` / `Parent`.
pub fn parse_mssql_showplan(rows: &[Vec<Value>]) -> Result<PlanTree, DriverError> {
    struct Row {
        id: i64,
        parent: i64,
        node: PlanNode,
    }
    let number = |r: &Vec<Value>, i: usize| -> Option<f64> {
        r.get(i)?.display().trim().parse::<f64>().ok()
    };
    let flat: Vec<Row> = rows
        .iter()
        .filter_map(|r| {
            let id = number(r, 2)? as i64;
            let text = r.first()?.display().into_owned();
            let label = text.trim_start_matches([' ', '|', '-']).trim().to_string();
            let argument = r
                .get(6)
                .map(|v| v.display().into_owned())
                .unwrap_or_default();
            let relation = argument.split("OBJECT:(").nth(1).and_then(|rest| {
                let end = rest.find(')')?;
                Some(rest[..end].to_string())
            });
            let physical = r
                .get(4)
                .map(|v| v.display().into_owned())
                .unwrap_or_default();
            Some(Row {
                id,
                parent: number(r, 3).unwrap_or(0.0) as i64,
                node: PlanNode {
                    kind: if physical.is_empty() || physical == "NULL" {
                        label.split_whitespace().next().unwrap_or("").to_string()
                    } else {
                        physical
                    },
                    label,
                    relation,
                    cost: number(r, 12).map(|c| (0.0, c)),
                    rows_estimate: number(r, 8),
                    ..PlanNode::default()
                },
            })
        })
        .collect();
    fn build(flat: &[Row], parent: i64) -> Vec<PlanNode> {
        flat.iter()
            .filter(|r| r.parent == parent)
            .map(|r| PlanNode {
                children: build(flat, r.id),
                ..r.node.clone()
            })
            .collect()
    }
    // A node is its own parent's child only below the root: the root has parent 0.
    let mut top = build(&flat, 0);
    let root = match top.len() {
        0 => return Err(bad("the plan is empty")),
        1 => top.remove(0),
        _ => PlanNode {
            label: "Plan".into(),
            kind: "Plan".into(),
            children: top,
            ..PlanNode::default()
        },
    };
    Ok(PlanTree {
        root,
        planning_ms: None,
        execution_ms: None,
        raw: rows
            .iter()
            .filter_map(|r| r.first().map(|v| v.display().into_owned()))
            .collect::<Vec<_>>()
            .join("\n"),
        format: "text".into(),
    })
}

/// SQLite `EXPLAIN QUERY PLAN` rows: `(id, parent, detail)`.
pub fn parse_sqlite_rows(rows: &[(i64, i64, String)]) -> Result<PlanTree, DriverError> {
    fn build(rows: &[(i64, i64, String)], parent: i64) -> Vec<PlanNode> {
        rows.iter()
            .filter(|(_, p, _)| *p == parent)
            .map(|(id, _, detail)| {
                let relation = detail
                    .split_whitespace()
                    .skip_while(|w| !matches!(*w, "SCAN" | "SEARCH"))
                    .nth(1)
                    .map(String::from);
                PlanNode {
                    kind: detail.split_whitespace().next().unwrap_or("").to_string(),
                    label: detail.clone(),
                    relation,
                    children: build(rows, *id),
                    ..PlanNode::default()
                }
            })
            .collect()
    }
    let mut top = build(rows, 0);
    let root = match top.len() {
        0 => return Err(bad("the plan is empty")),
        1 if !top[0].children.is_empty() => top.remove(0),
        _ => PlanNode {
            label: "Query plan".into(),
            kind: "QUERY PLAN".into(),
            children: top,
            ..PlanNode::default()
        },
    };
    Ok(PlanTree {
        root,
        planning_ms: None,
        execution_ms: None,
        raw: rows
            .iter()
            .map(|(id, p, d)| format!("{id} {p} {d}"))
            .collect::<Vec<_>>()
            .join("\n"),
        format: "text".into(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn oracle_plan_table_becomes_a_tree() {
        let text = "Plan hash value: 1\n\n\
            ---------------------------------------------------------\n\
            | Id  | Operation          | Name | Rows  | Bytes | Cost (%CPU)| Time     |\n\
            ---------------------------------------------------------\n\
            |   0 | SELECT STATEMENT   |      |    14 |   532 |     3   (0)| 00:00:01 |\n\
            |*  1 |  HASH JOIN         |      |    14 |   532 |     3   (0)| 00:00:01 |\n\
            |   2 |   TABLE ACCESS FULL| EMP  |   12K |   532 |     3   (0)| 00:00:01 |\n\
            |   3 |   TABLE ACCESS FULL| DEPT |     4 |    88 |     3   (0)| 00:00:01 |\n\
            ---------------------------------------------------------\n";
        let plan = parse_oracle_text(text).unwrap();
        assert_eq!(plan.root.kind, "SELECT STATEMENT");
        let join = &plan.root.children[0];
        assert_eq!(join.kind, "HASH JOIN");
        assert_eq!(join.cost, Some((0.0, 3.0)));
        assert_eq!(join.children.len(), 2);
        assert_eq!(join.children[0].relation.as_deref(), Some("EMP"));
        assert_eq!(join.children[0].rows_estimate, Some(12_000.0));
        assert_eq!(join.children[1].relation.as_deref(), Some("DEPT"));
    }

    #[test]
    fn mssql_showplan_rows_become_a_tree() {
        let row = |text: &str, id: i64, parent: i64, op: Option<&str>, arg: &str, rows: f64| {
            let mut r = vec![
                Value::Text(text.into()),
                Value::Int(1),
                Value::Int(id),
                Value::Int(parent),
                op.map_or(Value::Null, |o| Value::Text(o.into())),
                Value::Null,
                Value::Text(arg.into()),
                Value::Null,
                Value::Float(rows),
                Value::Float(0.0),
                Value::Float(0.0),
                Value::Int(10),
                Value::Float(0.5),
            ];
            r.push(Value::Null);
            r
        };
        let rows = vec![
            row("SELECT * FROM t", 1, 0, None, "", 5.0),
            row(
                "  |--Table Scan(OBJECT:([db].[dbo].[t]))",
                2,
                1,
                Some("Table Scan"),
                "OBJECT:([db].[dbo].[t])",
                5.0,
            ),
        ];
        let plan = parse_mssql_showplan(&rows).unwrap();
        assert_eq!(plan.root.label, "SELECT * FROM t");
        assert_eq!(plan.root.children.len(), 1);
        let scan = &plan.root.children[0];
        assert_eq!(scan.kind, "Table Scan");
        assert_eq!(scan.relation.as_deref(), Some("[db].[dbo].[t]"));
        assert_eq!(scan.cost, Some((0.0, 0.5)));
        assert_eq!(scan.rows_estimate, Some(5.0));
        assert!(parse_mssql_showplan(&[]).is_err());
    }

    use super::*;

    const PG: &str = r#"[{"Plan": {"Node Type": "Nested Loop", "Startup Cost": 0.0, "Total Cost": 30.0,
      "Plan Rows": 10, "Actual Rows": 8, "Actual Loops": 1, "Actual Total Time": 5.0,
      "Plans": [
        {"Node Type": "Seq Scan", "Relation Name": "big", "Startup Cost": 0.0, "Total Cost": 20.0,
         "Plan Rows": 100, "Actual Rows": 100, "Actual Loops": 1, "Actual Total Time": 3.0,
         "Filter": "(x > 1)", "Shared Hit Blocks": 7},
        {"Node Type": "Index Scan", "Relation Name": "small", "Index Name": "small_pk",
         "Startup Cost": 0.1, "Total Cost": 0.5, "Plan Rows": 1, "Actual Rows": 1,
         "Actual Loops": 4, "Actual Total Time": 0.25}]},
     "Planning Time": 0.3, "Execution Time": 5.5}]"#;

    #[test]
    fn pg_json_parsed_with_self_times() {
        let plan = parse_pg_json(PG).unwrap();
        assert_eq!(plan.root.kind, "Nested Loop");
        assert_eq!(plan.planning_ms, Some(0.3));
        assert_eq!(plan.execution_ms, Some(5.5));
        let seq = &plan.root.children[0];
        assert_eq!(seq.relation.as_deref(), Some("big"));
        assert_eq!(seq.cost, Some((0.0, 20.0)));
        assert_eq!(seq.buffers, [("Shared Hit Blocks".to_string(), 7)]);
        assert!(seq.extra.contains(&("Filter".into(), "(x > 1)".into())));
        // Time is per loop: 0.25 ms × 4 loops.
        assert_eq!(plan.root.children[1].time_ms, Some(1.0));
        // The join itself: 5.0 − (3.0 + 1.0).
        assert_eq!(plan.root.self_time_ms, Some(1.0));
        assert!(parse_pg_json("[]").is_err());
    }

    #[test]
    fn explain_check_accepts_one_query() {
        let syntax = SqlSyntax::ansi();
        assert_eq!(explain_check(&syntax, " SELECT 1; ").unwrap(), "SELECT 1");
        assert!(explain_check(&syntax, "UPDATE t SET a = 1").is_ok());
        for bad in [
            "SELECT 1; SELECT 2",
            "CREATE TABLE t (a int)",
            "DROP TABLE t",
            "BEGIN",
            "",
        ] {
            let e = explain_check(&syntax, bad).unwrap_err();
            assert_eq!(e.kind, ErrorKind::Unsupported, "{bad}");
            assert!(!e.message.contains("Error"), "{}", e.message);
        }
    }

    #[test]
    fn explain_parsers_read_rows() {
        let rows = vec![vec![Value::Text(PG.into())]];
        assert_eq!(
            ExplainParser::PgJson.parse(&rows).unwrap().root.kind,
            "Nested Loop"
        );
        let sqlite = vec![vec![
            Value::Int(2),
            Value::Int(0),
            Value::Int(0),
            Value::Text("SCAN t".into()),
        ]];
        let plan = ExplainParser::SqliteRows.parse(&sqlite).unwrap();
        assert_eq!(plan.root.children[0].label, "SCAN t");
    }

    #[test]
    fn plan_hottest() {
        let plan = parse_pg_json(PG).unwrap();
        // By self time: the scan of `big` (3.0), then the join and the index scan (1.0 each).
        assert_eq!(plan.hottest(1), vec![vec![0]]);
        assert_eq!(plan.hottest(3), vec![vec![0], vec![], vec![1]]);
        assert_eq!(plan.node(&[1]).unwrap().relation.as_deref(), Some("small"));
        // Without timings the estimate decides: own cost = total − children's.
        let estimate = PG.replace("\"Actual Total Time\"", "\"X\"");
        let plan = parse_pg_json(&estimate).unwrap();
        assert_eq!(plan.hottest(1), vec![vec![0]]);
    }

    #[test]
    fn mysql_tree_parsed() {
        let text = "-> Nested loop inner join  (cost=2.5 rows=4) (actual time=0.1..0.9 rows=4 loops=1)\n\
                    \x20   -> Table scan on a  (cost=0.5 rows=2) (actual time=0.02..0.1 rows=2 loops=1)\n\
                    \x20   -> Single-row index lookup on b using PRIMARY (id=a.id)  (cost=0.3 rows=1) (actual time=0.1..0.2 rows=1 loops=2)\n";
        let plan = parse_mysql_tree(text).unwrap();
        assert_eq!(plan.root.label, "Nested loop inner join");
        assert_eq!(plan.root.cost, Some((0.0, 2.5)));
        assert_eq!(plan.root.children.len(), 2);
        let scan = &plan.root.children[0];
        assert_eq!(scan.relation.as_deref(), Some("a"));
        assert_eq!(scan.rows_actual, Some(2.0));
        assert_eq!(plan.root.children[1].loops, Some(2.0));
        assert_eq!(plan.execution_ms, Some(0.9));
        assert!(parse_mysql_tree("nothing").is_err());
    }

    #[test]
    fn mysql_json_parsed() {
        let text = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "3.40"},
          "nested_loop": [
            {"table": {"table_name": "t1", "access_type": "ALL", "rows_examined_per_scan": 10,
                       "cost_info": {"prefix_cost": "1.0"}}},
            {"table": {"table_name": "t2", "access_type": "ref", "key": "idx",
                       "rows_examined_per_scan": 2, "cost_info": {"prefix_cost": "3.4"}}}]}}"#;
        let plan = parse_mysql_json(text).unwrap();
        assert_eq!(plan.root.cost, Some((0.0, 3.4)));
        assert_eq!(plan.root.children.len(), 2);
        assert_eq!(plan.root.children[1].relation.as_deref(), Some("t2"));
        assert!(
            plan.root.children[1]
                .extra
                .contains(&("key".into(), "idx".into()))
        );
    }

    #[test]
    fn sqlite_plan_rows_parsed() {
        let rows = vec![
            (
                2,
                0,
                "SEARCH orders USING INDEX idx (user_id=?)".to_string(),
            ),
            (3, 0, "SCAN users".to_string()),
            (5, 3, "USE TEMP B-TREE FOR ORDER BY".to_string()),
        ];
        let plan = parse_sqlite_rows(&rows).unwrap();
        assert_eq!(plan.root.label, "Query plan");
        assert_eq!(plan.root.children.len(), 2);
        assert_eq!(plan.root.children[0].relation.as_deref(), Some("orders"));
        assert_eq!(plan.root.children[1].children.len(), 1);
        assert!(parse_sqlite_rows(&[]).is_err());
    }

    const ANALYZED: &str = r#"[{"Plan": {"Node Type": "Nested Loop", "Startup Cost": 0.0,
      "Total Cost": 30.0, "Plan Rows": 10, "Actual Rows": 8, "Actual Loops": 1,
      "Actual Total Time": 5.0, "Plans": [
        {"Node Type": "Seq Scan", "Relation Name": "big", "Startup Cost": 0.0, "Total Cost": 20.0,
         "Plan Rows": 100, "Actual Rows": 15000, "Actual Loops": 1, "Actual Total Time": 3.0,
         "Shared Hit Blocks": 2, "Filter": "(x > 1)"},
        {"Node Type": "Index Scan", "Relation Name": "small", "Startup Cost": 0.1,
         "Total Cost": 0.5, "Plan Rows": 1, "Actual Rows": 1, "Actual Loops": 40000,
         "Actual Total Time": 0.25},
        {"Node Type": "Sort", "Startup Cost": 0.0, "Total Cost": 0.5, "Plan Rows": 10,
         "Actual Rows": 10, "Actual Loops": 1, "Actual Total Time": 0.1,
         "Temp Read Blocks": 12, "Temp Written Blocks": 12}]},
      "Planning Time": 0.3, "Execution Time": 5.5}]"#;

    #[test]
    fn insights_count_the_operators_the_depth_and_the_roles() {
        let plan = parse_pg_json(ANALYZED).unwrap();
        let insights = plan.insights();
        assert_eq!(insights.nodes, 4);
        assert_eq!(insights.depth, 2);
        assert_eq!(insights.joins, 1);
        assert_eq!(insights.scans, 2);
        assert_eq!(insights.sorts, 1);
        assert_eq!(insights.aggregates, 0);
        assert!(insights.timed);
        assert_eq!(plan.root.title(), "Nested Loop");
        assert_eq!(plan.root.children[0].title(), "Seq Scan on big");
        assert_eq!(plan.root.children[0].role(), PlanRole::Scan);
        assert_eq!(plan.root.role(), PlanRole::Join);
    }

    #[test]
    fn insights_flag_a_bad_estimate_a_full_scan_many_loops_and_spills() {
        let plan = parse_pg_json(ANALYZED).unwrap();
        let insights = plan.insights();
        let messages: Vec<&str> = insights.issues.iter().map(|i| i.message.as_str()).collect();
        assert!(
            messages
                .iter()
                .any(|m| m.contains("100 rows estimated, 15,000 read (150× more)")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Seq Scan on big reads 15,000 rows (full scan)")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("Index Scan on small runs 40,000 times")),
            "{messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.contains("spills to disk")),
            "{messages:?}"
        );
        // Both counts are per loop: 1 estimated and 1 read 40,000 times is not a bad
        // estimate, only a lot of loops.
        assert!(
            !messages
                .iter()
                .any(|m| m.contains("Index Scan on small: 1 rows estimated")),
            "{messages:?}"
        );
        // Warnings name the node they are about, and the paths point at it.
        let spill = insights
            .issues
            .iter()
            .find(|i| i.message.contains("spills to disk"))
            .unwrap();
        assert_eq!(spill.node, "Sort");
        assert_eq!(spill.path, vec![2]);
    }

    #[test]
    fn a_plan_without_timings_gets_a_note_not_a_warning() {
        let plan = parse_pg_json(
            r#"[{"Plan": {"Node Type": "Seq Scan", "Relation Name": "t",
                 "Startup Cost": 0.0, "Total Cost": 12.0, "Plan Rows": 500}}]"#,
        )
        .unwrap();
        let insights = plan.insights();
        assert!(!insights.timed);
        assert_eq!(insights.issues.len(), 1);
        assert_eq!(insights.issues[0].level, PlanIssueLevel::Note);
        assert!(insights.issues[0].message.contains("Estimated plan"));
        // The estimate alone is too small to be worth a scan warning.
        assert!(
            insights
                .issues
                .iter()
                .all(|i| i.level != PlanIssueLevel::Warning)
        );
    }

    #[test]
    fn mysql_and_sqlite_full_scans_are_recognized() {
        let mysql = parse_mysql_json(
            r#"{"query_block": {"select_id": 1, "table": {"table_name": "orders",
                 "access_type": "ALL", "rows_examined_per_scan": 20000}}}"#,
        )
        .unwrap();
        let insights = mysql.insights();
        assert_eq!(insights.scans, 1);
        assert!(
            insights
                .issues
                .iter()
                .any(|i| i.message == "ALL on orders is expected to read 20,000 rows (full scan)"),
            "{:?}",
            insights.issues
        );

        let sqlite = parse_sqlite_rows(&[(1, 0, "SCAN orders".into())]).unwrap();
        let insights = sqlite.insights();
        assert_eq!(insights.scans, 1);
        assert_eq!(insights.sorts, 0);
        assert!(!insights.timed);
    }

    #[test]
    fn a_sqlite_sort_under_a_scan_is_counted() {
        let plan = parse_sqlite_rows(&[
            (1, 0, "SCAN orders".into()),
            (2, 1, "USE TEMP B-TREE FOR ORDER BY".into()),
        ])
        .unwrap();
        let insights = plan.insights();
        assert_eq!(insights.scans, 1);
        assert_eq!(insights.sorts, 1);
        assert_eq!(insights.depth, 2);
    }
}
