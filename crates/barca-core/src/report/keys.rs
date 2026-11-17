//! A partitioned node's keys as `barca status NODE --keys`, `--grid` and `--key` print them:
//! the same text and JSON on this machine and from a server, which serves the grid
//! (`GET /assets/{name}/partitions/grid`) and one key (`.../partitions/key`) these read.
use super::*;
use crate::partition_grid::{KeyFilter, PartitionGrid, PartitionKeyDetail, state};

fn out(rendered: &mut RenderedOutput, text: String) {
    rendered.chunks.push(OutputChunk::Stdout(text));
}

fn pretty(value: &impl serde::Serialize) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    text
}

/// A state's word in a sentence: `never run`, not `never_run`.
fn word(name: &str) -> String {
    name.replace('_', " ")
}

/// `6 keys · 1 failed · 2 never run · 3 fresh`, most urgent first; `known: false` says so.
fn counts_line(grid: &PartitionGrid) -> String {
    if !grid.known {
        return format!(
            "{}: keys not known yet (they come from an upstream that has not run)",
            grid.id
        );
    }
    let mut parts = vec![plural(grid.keys() as u64, "key")];
    for (code, name) in state::NAMED {
        let n = grid.counts.of(code);
        if n > 0 {
            parts.push(format!("{n} {}", word(name)));
        }
    }
    if grid.counts.empty > 0 {
        parts.push(format!("{} empty (0 rows)", grid.counts.empty));
    }
    format!("{}: {}", grid.id, parts.join(" · "))
}

fn size_cell(bytes: Option<i64>) -> String {
    match bytes {
        None => "-".to_string(),
        Some(b) if b < 1024 => format!("{b} B"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        Some(b) if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        Some(b) => format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0)),
    }
}

fn dash<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "-".to_string(), |v| v.to_string())
}

/// Rows as a left-aligned table, each column as wide as its longest cell.
fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let widths: Vec<usize> = header
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain([h.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
            + "\n"
    };
    let mut text = line(header.to_vec());
    for row in rows {
        text.push_str(&line(row.iter().map(String::as_str).collect()));
    }
    text
}

/// `barca status NODE --keys`: the node's keys that match `filter`, each with its state and
/// latest attempt, at most `limit` of them. JSON: `{id, known, dimensions, counts, errors,
/// keys, total, truncated, hint?}`; `total` counts the keys that match, `counts` every key.
pub fn render_partition_keys(
    grid: &PartitionGrid,
    filter: &KeyFilter,
    limit: Option<usize>,
    json: bool,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    let mut rows = grid.key_rows(filter);
    let total = rows.len();
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    let page = Page::new(rows.len(), total);
    if json {
        let items = rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        let mut body = Map::new();
        body.insert("id".into(), grid.id.clone().into());
        body.insert("known".into(), grid.known.into());
        body.insert(
            "dimensions".into(),
            grid.dimensions
                .iter()
                .map(|d| Value::from(d.name.clone()))
                .collect(),
        );
        body.insert("counts".into(), serde_json::to_value(&grid.counts).unwrap());
        body.insert("errors".into(), serde_json::to_value(&grid.errors).unwrap());
        if let Value::Object(envelope) = page.envelope("keys", items, "keys") {
            body.extend(envelope);
        }
        out(&mut rendered, pretty(&Value::Object(body)));
        return rendered;
    }
    let mut text = counts_line(grid);
    text.push('\n');
    if !rows.is_empty() {
        text.push('\n');
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                vec![
                    r.key.clone(),
                    word(r.state),
                    r.attempted_at.as_deref().map_or("-".into(), utc_cell),
                    r.elapsed_seconds.map_or("-".into(), |s| format!("{s:.2}s")),
                    dash(r.rows),
                    size_cell(r.size_bytes),
                    dash(r.run_id.clone()),
                    r.error.clone().unwrap_or_default(),
                ]
            })
            .collect();
        text.push_str(&table(
            &[
                "KEY",
                "STATE",
                "LAST ATTEMPT",
                "ELAPSED",
                "ROWS",
                "SIZE",
                "RUN",
                "ERROR",
            ],
            &cells,
        ));
    } else if grid.known && !filter.states.is_empty() {
        text.push_str("no key is in the states asked for\n");
    }
    if !grid.errors.is_empty() {
        text.push_str("\nerrors of the failed keys:\n");
        for group in &grid.errors {
            text.push_str(&format!(
                "  {}: {}\n",
                plural(group.keys as u64, "key"),
                group.error
            ));
        }
    }
    out(&mut rendered, text);
    if let Some(note) = page.note(rows.len(), "keys") {
        rendered.chunks.push(OutputChunk::Stderr(note + "\n"));
    }
    rendered
}

/// The widest strip of states printed on one line.
const STRIP: usize = 100;

/// `barca status NODE --grid`: every key's state, one character each. JSON is the grid as
/// `GET /assets/{name}/partitions/grid` answers it, whole; the text has one row per
/// combination of every dimension but the longest (at most `limit` rows), each running
/// through the longest dimension's values.
pub fn render_partition_grid(
    grid: &PartitionGrid,
    limit: Option<usize>,
    json: bool,
) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if json {
        out(&mut rendered, pretty(grid));
        return rendered;
    }
    let mut text = counts_line(grid);
    text.push('\n');
    let mut blocks = grid.blocks();
    let total = blocks.len();
    if let Some(limit) = limit {
        blocks.truncate(limit);
    }
    let page = Page::new(blocks.len(), total);
    if let Some(along) = grid.dimensions.get(grid.along()) {
        let values = &along.values;
        let span = match values.len() {
            0 => String::new(),
            n if n <= 12 => values.join(", "),
            n => format!("{} … {} ({n} values)", values[0], values[n - 1]),
        };
        text.push_str(&format!("each row runs along {}: {span}\n", along.name));
        text.push_str(
            "c fresh  p pinned  s stale  n never run  f failed  b blocked  - not a key\n\n",
        );
        let states: Vec<char> = grid.states.chars().collect();
        let strip = |cells: &[usize]| -> String {
            cells
                .iter()
                .map(|c| states.get(*c).copied().unwrap_or(state::NONE))
                .collect()
        };
        let width = blocks
            .iter()
            .map(|b| b.label.chars().count())
            .max()
            .unwrap_or(0);
        for block in &blocks {
            if block.cells.len() <= STRIP {
                let label = if block.label.is_empty() {
                    along.name.as_str()
                } else {
                    block.label.as_str()
                };
                let width = width.max(label.chars().count());
                text.push_str(&format!("{label:<width$}  {}\n", strip(&block.cells)));
                continue;
            }
            // A long dimension wraps: each line starts at the value it names.
            if !block.label.is_empty() {
                text.push_str(&format!("{}\n", block.label));
            }
            let from = values
                .iter()
                .step_by(STRIP)
                .map(|v| v.chars().count())
                .max()
                .unwrap_or(0);
            for (line, cells) in block.cells.chunks(STRIP).enumerate() {
                text.push_str(&format!(
                    "  {:<from$}  {}\n",
                    values[line * STRIP],
                    strip(cells)
                ));
            }
        }
    }
    out(&mut rendered, text);
    if let Some(note) = page.note(blocks.len(), "rows") {
        rendered.chunks.push(OutputChunk::Stderr(note + "\n"));
    }
    rendered
}

/// `barca status NODE --key KEY`: one key across its chain, its attempts, what it wrote and
/// what it read. JSON is the detail as `GET /assets/{name}/partitions/key` answers it.
pub fn render_partition_key(detail: &PartitionKeyDetail, json: bool) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if json {
        out(&mut rendered, pretty(detail));
        return rendered;
    }
    let named = |code: &str| -> String {
        code.chars()
            .next()
            .and_then(state::name)
            .map_or_else(|| "no such key".to_string(), word)
    };
    let mut text = format!("{}[{}]: {}\n", detail.id, detail.key, named(&detail.state));
    if detail.chain.len() > 1 {
        text.push_str("\nthe same key in each node of its chain, upstream first:\n");
        let width = detail
            .chain
            .iter()
            .map(|c| c.id.chars().count())
            .max()
            .unwrap_or(0);
        for node in &detail.chain {
            text.push_str(&format!("  {:<width$}  {}\n", node.id, named(&node.state)));
        }
    }
    if detail.attempts.is_empty() {
        text.push_str("\nno attempt recorded\n");
    } else {
        text.push_str("\nattempts, newest first:\n");
        let cells: Vec<Vec<String>> = detail
            .attempts
            .iter()
            .map(|a| {
                vec![
                    a.status.clone(),
                    utc_cell(&a.created_at),
                    a.elapsed_seconds.map_or("-".into(), |s| format!("{s:.2}s")),
                    dash(a.rows),
                    size_cell(a.size_bytes),
                    dash(a.run_id.clone()),
                    a.error
                        .as_deref()
                        .and_then(|e| e.lines().next())
                        .unwrap_or("")
                        .to_string(),
                ]
            })
            .collect();
        for line in table(
            &["STATUS", "AT", "ELAPSED", "ROWS", "SIZE", "RUN", "ERROR"],
            &cells,
        )
        .lines()
        {
            text.push_str(&format!("  {line}\n"));
        }
        // The newest failure in full: the table has its first line only.
        if let Some(error) = detail
            .attempts
            .first()
            .filter(|a| a.status == "failed")
            .and_then(|a| a.error.as_deref())
            .filter(|e| e.lines().count() > 1)
        {
            text.push_str("\nlatest error:\n");
            for line in error.lines() {
                text.push_str(&format!("  {line}\n"));
            }
        }
    }
    let artifact = |a: &crate::partition_grid::KeyArtifact| -> String {
        match &a.artifact_path {
            None => format!("{}: no successful result", a.step),
            Some(path) => {
                let mut line = format!(
                    "{}: {path} ({}, {})",
                    a.step,
                    a.format.as_deref().unwrap_or("?"),
                    size_cell(a.size_bytes)
                );
                if let Some(shape) = &a.shape {
                    line.push_str(&format!(", {}", shape_cell(shape)));
                }
                line
            }
        }
    };
    match &detail.output {
        Some(output) => text.push_str(&format!("\nwrote:\n  {}\n", artifact(output))),
        None => text.push_str("\nwrote: no successful result\n"),
    }
    if !detail.inputs.is_empty() {
        text.push_str("\nreads:\n");
        for input in &detail.inputs {
            text.push_str(&format!("  {}\n", artifact(input)));
        }
    }
    out(&mut rendered, text);
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PartitionErrorGroup;
    use crate::partition_grid::{GridCounts, GridDimension};

    /// `day` (2 values) × `region` (3): the last dimension varies fastest.
    fn grid() -> PartitionGrid {
        PartitionGrid {
            id: "p.py:events".into(),
            chain: vec!["p.py:events".into()],
            known: true,
            dimensions: vec![
                GridDimension {
                    name: "day".into(),
                    values: vec!["d1".into(), "d2".into()],
                },
                GridDimension {
                    name: "region".into(),
                    values: vec!["emea".into(), "amer".into(), "apac".into()],
                },
            ],
            states: "cfn-cs".into(),
            counts: GridCounts {
                fresh: 2,
                stale: 1,
                never_run: 1,
                failed: 1,
                ..Default::default()
            },
            attempted_base: 1_700_000_000,
            attempted_at: vec![Some(0), Some(60), None, None, Some(120), Some(180)],
            elapsed_ms: vec![Some(1500), Some(20), None, None, Some(10), Some(10)],
            size_bytes: vec![Some(2048), None, None, None, Some(10), Some(10)],
            rows: vec![Some(3), None, None, None, None, None],
            run: vec![Some(0), Some(1), None, None, Some(0), Some(0)],
            error: [(1, 0)].into_iter().collect(),
            errors: vec![PartitionErrorGroup {
                error: "ValueError: no rows for <region>".into(),
                keys: 1,
            }],
            runs: vec!["run-a".into(), "run-b".into()],
        }
    }

    fn stdout(rendered: RenderedOutput) -> String {
        rendered
            .chunks
            .into_iter()
            .filter_map(|c| match c {
                OutputChunk::Stdout(text) => Some(text),
                OutputChunk::Stderr(_) => None,
            })
            .collect()
    }

    #[test]
    fn keys_are_decoded_in_key_order_and_a_cell_that_is_not_a_key_is_left_out() {
        let rows = grid().key_rows(&KeyFilter::default());
        let keys: Vec<(&str, &str)> = rows.iter().map(|r| (r.key.as_str(), r.state)).collect();
        assert_eq!(
            keys,
            [
                ("day=d1,region=emea", "fresh"),
                ("day=d1,region=amer", "failed"),
                ("day=d1,region=apac", "never_run"),
                ("day=d2,region=amer", "fresh"),
                ("day=d2,region=apac", "stale"),
            ]
        );
        assert_eq!(
            rows[0].attempted_at.as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
        assert_eq!(rows[0].elapsed_seconds, Some(1.5));
        assert_eq!(rows[0].rows, Some(3));
        assert_eq!(rows[1].run_id.as_deref(), Some("run-b"));
        assert_eq!(
            rows[1].error.as_deref(),
            Some("ValueError: no rows for <region>")
        );
        assert!(rows[2].attempted_at.is_none() && rows[2].run_id.is_none());
    }

    #[test]
    fn a_state_filter_bounds_the_list_and_total_counts_what_matches() {
        let filter = KeyFilter::parse(&["failed".into(), "never_run".into()]).unwrap();
        let body: Value = serde_json::from_str(&stdout(render_partition_keys(
            &grid(),
            &filter,
            Some(1),
            true,
        )))
        .unwrap();
        assert_eq!(body["total"], 2);
        assert_eq!(body["truncated"], true);
        assert_eq!(body["keys"].as_array().unwrap().len(), 1);
        assert_eq!(body["keys"][0]["key"], "day=d1,region=amer");
        assert_eq!(body["counts"]["fresh"], 2);
        assert_eq!(body["dimensions"], serde_json::json!(["day", "region"]));
        assert!(body["hint"].as_str().unwrap().contains("--all"));
        let err = KeyFilter::parse(&["cached".into()]).unwrap_err();
        assert!(err.contains("unknown key state 'cached'") && err.contains("never_run"));
    }

    #[test]
    fn the_grid_runs_along_the_longest_dimension_one_row_per_other_value() {
        let text = stdout(render_partition_grid(&grid(), None, false));
        assert!(text.starts_with(
            "p.py:events: 5 keys · 1 failed · 1 stale · 1 never run · 2 fresh\n\
             each row runs along region: emea, amer, apac\n"
        ));
        assert!(text.ends_with("day=d1  cfn\nday=d2  -cs\n"), "{text}");
    }

    #[test]
    fn the_grid_json_is_the_grid_the_route_serves() {
        let text = stdout(render_partition_grid(&grid(), Some(1), true));
        let back: PartitionGrid = serde_json::from_str(&text).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(grid()).unwrap()
        );
    }

    #[test]
    fn a_long_dimension_wraps_with_the_value_each_line_starts_at() {
        let n = 250;
        let g = PartitionGrid {
            dimensions: vec![GridDimension {
                name: "day".into(),
                values: (0..n).map(|i| format!("v{i:03}")).collect(),
            }],
            states: "c".repeat(n),
            counts: GridCounts {
                fresh: n,
                ..Default::default()
            },
            attempted_at: vec![None; n],
            elapsed_ms: vec![None; n],
            size_bytes: vec![None; n],
            rows: vec![None; n],
            run: vec![None; n],
            error: Default::default(),
            errors: vec![],
            runs: vec![],
            ..grid()
        };
        let text = stdout(render_partition_grid(&g, None, false));
        let lines: Vec<&str> = text.lines().rev().take(3).collect();
        assert_eq!(lines[2], format!("  v000  {}", "c".repeat(100)));
        assert_eq!(lines[0], format!("  v200  {}", "c".repeat(50)));
    }
}
