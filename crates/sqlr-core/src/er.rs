//! ER diagrams (spec §8.6): the model of tables and foreign keys, a deterministic layered
//! layout, and the SVG export. Diagrams are views of a schema, never editors of it.

use std::collections::{BTreeMap, BTreeSet};

use crate::compare::SchemaSnapshot;
use crate::schema::{ConstraintDef, ObjectDef};
use crate::table::TableRef;

/// A column of a diagram table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErColumn {
    /// Column name.
    pub name: String,
    /// The type name as the driver reports it.
    pub type_name: String,
    /// Whether the column is part of the primary key.
    pub pk: bool,
    /// Whether the column is part of a foreign key.
    pub fk: bool,
    /// Whether the column accepts NULL.
    pub nullable: bool,
}

/// A table box of the diagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErTable {
    /// The table the box stands for.
    pub table: TableRef,
    /// Its columns, in ordinal order.
    pub columns: Vec<ErColumn>,
}

/// A foreign key: `from` references `to` (indexes into [`ErModel::tables`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErEdge {
    /// Index of the referencing table.
    pub from: usize,
    /// Index of the referenced table.
    pub to: usize,
    /// `(column of from, column of to)`.
    pub columns: Vec<(String, String)>,
    /// The referencing columns are unique: at most one row per target row.
    pub unique: bool,
    /// Whether a referencing column accepts NULL, so the reference is optional.
    pub nullable: bool,
}

/// The tables and foreign keys of a diagram.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErModel {
    /// The tables, in name order.
    pub tables: Vec<ErTable>,
    /// The foreign keys between them.
    pub edges: Vec<ErEdge>,
}

impl ErModel {
    /// The tables of `snapshot` (only `only`, by name, when given) and the foreign keys
    /// between them, in name order.
    pub fn from_snapshot(snapshot: &SchemaSnapshot, only: Option<&[String]>) -> Self {
        let mut tables: Vec<&crate::schema::TableDef> = snapshot
            .objects
            .iter()
            .filter_map(|o| match o {
                ObjectDef::Table(t) => Some(t),
                _ => None,
            })
            .filter(|t| only.is_none_or(|names| names.contains(&t.table.name)))
            .collect();
        tables.sort_by(|a, b| a.table.name.cmp(&b.table.name));
        let index: BTreeMap<&str, usize> = tables
            .iter()
            .enumerate()
            .map(|(i, t)| (t.table.name.as_str(), i))
            .collect();
        let mut model = ErModel::default();
        for t in &tables {
            let pk: BTreeSet<&str> = t
                .primary_key()
                .map(|c| c.iter().map(String::as_str).collect())
                .unwrap_or_default();
            let fk: BTreeSet<&str> = t
                .constraints
                .iter()
                .filter_map(|c| match c {
                    ConstraintDef::ForeignKey { columns, .. } => Some(columns),
                    _ => None,
                })
                .flatten()
                .map(String::as_str)
                .collect();
            model.tables.push(ErTable {
                table: t.table.clone(),
                columns: t
                    .columns
                    .iter()
                    .map(|c| ErColumn {
                        name: c.name.clone(),
                        type_name: c.type_name.clone(),
                        pk: pk.contains(c.name.as_str()),
                        fk: fk.contains(c.name.as_str()),
                        nullable: c.nullable,
                    })
                    .collect(),
            });
        }
        for (from, t) in tables.iter().enumerate() {
            for c in &t.constraints {
                let ConstraintDef::ForeignKey {
                    columns,
                    target,
                    target_columns,
                    ..
                } = c
                else {
                    continue;
                };
                let Some(&to) = index.get(target.name.as_str()) else {
                    continue;
                };
                let unique = t.constraints.iter().any(|u| match u {
                    ConstraintDef::PrimaryKey { columns: k, .. }
                    | ConstraintDef::Unique { columns: k, .. } => k == columns,
                    _ => false,
                });
                model.edges.push(ErEdge {
                    from,
                    to,
                    columns: columns
                        .iter()
                        .cloned()
                        .zip(
                            target_columns
                                .iter()
                                .cloned()
                                .chain(std::iter::repeat(String::new())),
                        )
                        .collect(),
                    unique,
                    nullable: columns
                        .iter()
                        .any(|n| t.column(n).is_some_and(|c| c.nullable)),
                });
            }
        }
        model
    }
}

/// Box positions (top-left) and sizes, in the diagram's own units.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ErLayout {
    /// Top-left corner of each table box, indexed like [`ErModel::tables`].
    pub positions: Vec<(f32, f32)>,
    /// Width and height of each table box, indexed like [`ErModel::tables`].
    pub sizes: Vec<(f32, f32)>,
}

const LAYER_GAP: f32 = 80.0;
const ROW_GAP: f32 = 30.0;
const BARYCENTER_PASSES: usize = 4;

/// A layered layout (Sugiyama style), deterministic: the same model gives the same boxes.
/// Referenced tables sit in earlier layers than their referrers (cycles are broken by
/// reversing the edge from the later table name); within a layer, four barycenter passes
/// order the boxes by their neighbours; boxes are sized by `measure`.
pub fn layout(model: &ErModel, measure: &dyn Fn(&ErTable) -> (f32, f32)) -> ErLayout {
    let n = model.tables.len();
    let sizes: Vec<(f32, f32)> = model.tables.iter().map(measure).collect();
    if n == 0 {
        return ErLayout::default();
    }
    // Directed edges parent → child over distinct pairs, self references left out.
    let name = |i: usize| model.tables[i].table.name.as_str();
    let mut pairs: BTreeSet<(usize, usize)> = BTreeSet::new();
    for e in &model.edges {
        if e.from != e.to {
            pairs.insert((e.to, e.from));
        }
    }
    // Break cycles: a DFS in name order; an edge back to a node on the stack is reversed.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| name(a).cmp(name(b)));
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(p, c) in &pairs {
        out[p].push(c);
    }
    for list in &mut out {
        list.sort_by(|&a, &b| name(a).cmp(name(b)));
    }
    let mut state = vec![0u8; n]; // 0 new, 1 on the stack, 2 done
    let mut dag: BTreeSet<(usize, usize)> = BTreeSet::new();
    fn visit(v: usize, out: &[Vec<usize>], state: &mut [u8], dag: &mut BTreeSet<(usize, usize)>) {
        state[v] = 1;
        for &w in &out[v] {
            match state[w] {
                1 => {
                    // A back edge closes a cycle: keep it the other way round.
                    dag.insert((w, v));
                }
                0 => {
                    dag.insert((v, w));
                    visit(w, out, state, dag);
                }
                _ => {
                    dag.insert((v, w));
                }
            }
        }
        state[v] = 2;
    }
    for &v in &order {
        if state[v] == 0 {
            visit(v, &out, &mut state, &mut dag);
        }
    }
    // Reversing can still leave two-node cycles in `dag` (both directions): drop the one
    // from the later name.
    let both: Vec<(usize, usize)> = dag
        .iter()
        .copied()
        .filter(|&(a, b)| dag.contains(&(b, a)) && name(a) > name(b))
        .collect();
    for e in both {
        dag.remove(&e);
    }
    // Longest-path layers, in a topological sweep.
    let mut incoming = vec![0usize; n];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(p, c) in &dag {
        incoming[c] += 1;
        children[p].push(c);
    }
    let mut layer = vec![0usize; n];
    let mut ready: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&v| incoming[v] == 0)
        .collect();
    let mut placed = 0;
    while let Some(v) = ready.pop() {
        placed += 1;
        for &c in &children[v] {
            layer[c] = layer[c].max(layer[v] + 1);
            incoming[c] -= 1;
            if incoming[c] == 0 {
                ready.push(c);
            }
        }
    }
    debug_assert_eq!(placed, n, "the graph is acyclic by construction");
    let depth = layer.iter().copied().max().unwrap_or(0) + 1;
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for &v in &order {
        columns[layer[v]].push(v);
    }
    // Barycenter ordering over the original (undirected) neighbours.
    let mut neighbours: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(a, b) in &pairs {
        neighbours[a].push(b);
        neighbours[b].push(a);
    }
    let mut rank = vec![0f32; n];
    let set_ranks = |columns: &[Vec<usize>], rank: &mut [f32]| {
        for col in columns {
            for (i, &v) in col.iter().enumerate() {
                rank[v] = i as f32;
            }
        }
    };
    set_ranks(&columns, &mut rank);
    for pass in 0..BARYCENTER_PASSES {
        let layers: Vec<usize> = if pass % 2 == 0 {
            (1..depth).collect()
        } else {
            (0..depth.saturating_sub(1)).rev().collect()
        };
        for l in layers {
            let reference = if pass % 2 == 0 { l - 1 } else { l + 1 };
            let mut keyed: Vec<(f32, usize)> = columns[l]
                .iter()
                .map(|&v| {
                    let linked: Vec<f32> = neighbours[v]
                        .iter()
                        .filter(|&&w| layer[w] == reference)
                        .map(|&w| rank[w])
                        .collect();
                    let key = if linked.is_empty() {
                        rank[v]
                    } else {
                        linked.iter().sum::<f32>() / linked.len() as f32
                    };
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| name(a.1).cmp(name(b.1))));
            columns[l] = keyed.into_iter().map(|(_, v)| v).collect();
            set_ranks(&columns, &mut rank);
        }
    }
    // Coordinates: layers left to right, boxes stacked top to bottom.
    let mut positions = vec![(0f32, 0f32); n];
    let mut x = 0f32;
    for col in &columns {
        let width = col.iter().map(|&v| sizes[v].0).fold(0f32, f32::max);
        let mut y = 0f32;
        for &v in col {
            positions[v] = (x, y);
            y += sizes[v].1 + ROW_GAP;
        }
        x += width + LAYER_GAP;
    }
    ErLayout { positions, sizes }
}

/// Colors of the SVG export, as `#rrggbb`.
#[derive(Clone, Debug)]
pub struct ErColors {
    /// Canvas fill.
    pub background: String,
    /// Table box body fill.
    pub box_fill: String,
    /// Table header fill.
    pub header_fill: String,
    /// Box and header outline.
    pub border: String,
    /// Main text.
    pub text: String,
    /// Secondary text, such as column types.
    pub muted: String,
    /// Foreign key lines.
    pub edge: String,
}

impl Default for ErColors {
    fn default() -> Self {
        Self {
            background: "#ffffff".into(),
            box_fill: "#f6f7f9".into(),
            header_fill: "#dde3ee".into(),
            border: "#8a94a6".into(),
            text: "#1d2330".into(),
            muted: "#667085".into(),
            edge: "#3b6fd4".into(),
        }
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The diagram as SVG: boxes with their columns (primary and foreign keys marked), elbow
/// edges with a crow's foot at the many side.
pub fn to_svg(model: &ErModel, layout: &ErLayout, colors: &ErColors) -> String {
    const HEADER: f32 = 26.0;
    const LINE: f32 = 18.0;
    let pad = 20.0;
    let (mut w, mut h) = (0f32, 0f32);
    for (p, s) in layout.positions.iter().zip(&layout.sizes) {
        w = w.max(p.0 + s.0);
        h = h.max(p.1 + s.1);
    }
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\" \
         font-family=\"monospace\" font-size=\"12\">\n<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>\n",
        w + 2.0 * pad,
        h + 2.0 * pad,
        -pad,
        -pad,
        w + 2.0 * pad,
        h + 2.0 * pad,
        -pad,
        -pad,
        w + 2.0 * pad,
        h + 2.0 * pad,
        colors.background
    );
    for e in &model.edges {
        let (Some(a), Some(b)) = (layout.positions.get(e.from), layout.positions.get(e.to)) else {
            continue;
        };
        let (sa, sb) = (layout.sizes[e.from], layout.sizes[e.to]);
        let (x1, y1, x2, y2);
        if e.from == e.to {
            // A self reference loops out of the right edge.
            x1 = a.0 + sa.0;
            y1 = a.1 + HEADER / 2.0;
            svg.push_str(&format!(
                "<path d=\"M {x1} {y1} h 20 v 24 h -20\" fill=\"none\" stroke=\"{}\"/>\n",
                colors.edge
            ));
            continue;
        }
        if a.0 <= b.0 {
            (x1, x2) = (a.0 + sa.0, b.0);
        } else {
            (x1, x2) = (a.0, b.0 + sb.0);
        }
        y1 = a.1 + sa.1 / 2.0;
        y2 = b.1 + sb.1 / 2.0;
        let mid = (x1 + x2) / 2.0;
        svg.push_str(&format!(
            "<path d=\"M {x1} {y1} H {mid} V {y2} H {x2}\" fill=\"none\" stroke=\"{}\"/>\n",
            colors.edge
        ));
        // The crow's foot sits at the referencing table unless the key is unique.
        if !e.unique {
            let dir = if x2 > x1 { 1.0 } else { -1.0 };
            svg.push_str(&format!(
                "<path d=\"M {x1} {y1} l {} -6 M {x1} {y1} l {} 6\" stroke=\"{}\"/>\n",
                dir * 10.0,
                dir * 10.0,
                colors.edge
            ));
        }
    }
    for (i, t) in model.tables.iter().enumerate() {
        let (x, y) = layout.positions[i];
        let (bw, bh) = layout.sizes[i];
        svg.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"{bw}\" height=\"{bh}\" fill=\"{}\" stroke=\"{}\"/>\n\
             <rect x=\"{x}\" y=\"{y}\" width=\"{bw}\" height=\"{HEADER}\" fill=\"{}\" stroke=\"{}\"/>\n\
             <text x=\"{}\" y=\"{}\" fill=\"{}\" font-weight=\"bold\">{}</text>\n",
            colors.box_fill, colors.border, colors.header_fill, colors.border,
            x + 8.0, y + 17.0, colors.text, esc(&t.table.name)
        ));
        for (k, c) in t.columns.iter().enumerate() {
            let cy = y + HEADER + LINE * (k as f32 + 1.0) - 5.0;
            let mark = match (c.pk, c.fk) {
                (true, true) => "PK FK",
                (true, false) => "PK",
                (false, true) => "FK",
                _ => "",
            };
            svg.push_str(&format!(
                "<text x=\"{}\" y=\"{cy}\" fill=\"{}\">{}</text>\
                 <text x=\"{}\" y=\"{cy}\" fill=\"{}\">{}</text>\
                 <text x=\"{}\" y=\"{cy}\" fill=\"{}\" text-anchor=\"end\">{}</text>\n",
                x + 8.0,
                colors.muted,
                mark,
                x + 52.0,
                colors.text,
                esc(&c.name),
                x + bw - 8.0,
                colors.muted,
                esc(&c.type_name)
            ));
        }
    }
    svg.push_str("</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str, columns: usize) -> ErTable {
        ErTable {
            table: TableRef {
                database: None,
                schema: Some("s".into()),
                name: name.into(),
            },
            columns: (0..columns)
                .map(|i| ErColumn {
                    name: format!("c{i}"),
                    type_name: "int".into(),
                    pk: i == 0,
                    fk: false,
                    nullable: false,
                })
                .collect(),
        }
    }

    fn edge(from: usize, to: usize) -> ErEdge {
        ErEdge {
            from,
            to,
            columns: vec![("c1".into(), "c0".into())],
            unique: false,
            nullable: false,
        }
    }

    fn measure(t: &ErTable) -> (f32, f32) {
        (
            120.0 + t.table.name.len() as f32,
            26.0 + 18.0 * t.columns.len() as f32,
        )
    }

    fn overlaps(l: &ErLayout) -> Option<(usize, usize)> {
        for i in 0..l.positions.len() {
            for j in i + 1..l.positions.len() {
                let (a, b) = (l.positions[i], l.positions[j]);
                let (sa, sb) = (l.sizes[i], l.sizes[j]);
                if a.0 < b.0 + sb.0 && b.0 < a.0 + sa.0 && a.1 < b.1 + sb.1 && b.1 < a.1 + sa.1 {
                    return Some((i, j));
                }
            }
        }
        None
    }

    #[test]
    fn layout_deterministic_and_non_overlapping() {
        // A pseudo-random graph of 200 tables and 300 foreign keys, cycles included.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        let model = ErModel {
            tables: (0..200)
                .map(|i| table(&format!("t{i:03}"), 2 + i % 9))
                .collect(),
            edges: (0..300).map(|_| edge(next(200), next(200))).collect(),
        };
        let one = layout(&model, &measure);
        let two = layout(&model, &measure);
        assert_eq!(one, two, "the same model gives the same layout");
        assert_eq!(overlaps(&one), None);
        assert_eq!(one.positions.len(), 200);
    }

    #[test]
    fn er_cycles_and_self_loops() {
        // Names: a ⇄ b (a cycle), c → c (a self reference), z → y → x (a chain).
        let model = ErModel {
            tables: ["a", "b", "c", "x", "y", "z"].map(|n| table(n, 3)).to_vec(),
            edges: vec![edge(0, 1), edge(1, 0), edge(2, 2), edge(5, 4), edge(4, 3)],
        };
        let l = layout(&model, &measure);
        assert_eq!(overlaps(&l), None);
        // Referenced tables come first: x, then y, then z, left to right.
        assert!(l.positions[3].0 < l.positions[4].0);
        assert!(l.positions[4].0 < l.positions[5].0);
        // The cycle still separates its two tables.
        assert_ne!(l.positions[0].0, l.positions[1].0);
        // The self reference is drawn as a loop.
        let svg = to_svg(&model, &l, &ErColors::default());
        assert!(svg.contains("h 20 v 24 h -20"), "{svg}");
    }

    #[test]
    fn svg_escapes_names() {
        let model = ErModel {
            tables: vec![table("a<b>&\"c", 2)],
            edges: Vec::new(),
        };
        let l = layout(&model, &measure);
        let svg = to_svg(&model, &l, &ErColors::default());
        assert!(svg.starts_with("<svg "));
        assert!(svg.contains("a&lt;b&gt;&amp;&quot;c"), "{svg}");
        assert!(!svg.contains("a<b>"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }
}
