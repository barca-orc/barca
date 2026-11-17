//! A partitioned node's keys as `barca status NODE --keys`, `--overview`, `--grid` and `--key`
//! print them: the same text and JSON on this machine and from a server, which serves the page
//! of keys (`GET /assets/{name}/partitions/keys`), the overview (`.../partitions/overview`),
//! the grid (`.../partitions/grid`) and one key (`.../partitions/key`) these read.
use super::*;
use crate::key_query::{KeysOverview, KeysPage};
use crate::partition_grid::{GridCounts, PartitionGrid, PartitionKeyDetail, state};

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
fn counts_line(id: &str, known: bool, counts: &GridCounts) -> String {
    if !known {
        return format!("{id}: keys not known yet (they come from an upstream that has not run)");
    }
    let total: usize = state::NAMED.iter().map(|(code, _)| counts.of(*code)).sum();
    let mut parts = vec![plural(total as u64, "key")];
    for (code, name) in state::NAMED {
        let n = counts.of(code);
        if n > 0 {
            parts.push(format!("{n} {}", word(name)));
        }
    }
    if counts.empty > 0 {
        parts.push(format!("{} empty (0 rows)", counts.empty));
    }
    format!("{id}: {}", parts.join(" · "))
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

/// `barca status NODE --keys`: one page of the keys a query matches
/// ([`PartitionGrid::keys_page`]), each with its state and latest attempt. JSON is the page as
/// `GET /assets/{name}/partitions/keys` answers it: `{id, known, dimensions, counts, errors,
/// keys, total, truncated, offset, needs_run, selection, hint?}`; `total` counts the keys that
/// match, `counts` every key. `filtered`: the query narrows the keys.
pub fn render_partition_keys(page: &KeysPage, filtered: bool, json: bool) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if json {
        out(&mut rendered, pretty(page));
        return rendered;
    }
    let mut text = counts_line(&page.id, page.known, &page.counts);
    text.push('\n');
    if !page.keys.is_empty() {
        text.push('\n');
        let cells: Vec<Vec<String>> = page
            .keys
            .iter()
            .map(|r| {
                vec![
                    r.key.clone(),
                    word(&r.state),
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
    } else if page.known && filtered && page.total == 0 {
        text.push_str("no key matches\n");
    }
    if filtered && page.total > 0 {
        text.push_str(&format!(
            "\n{} match, {} of them to run\n",
            plural(page.total as u64, "key"),
            page.needs_run
        ));
        if let Some(selection) = page.selection.as_deref().filter(|_| page.needs_run > 0) {
            text.push_str(&format!(
                "run them: barca get {} --partition '{selection}'\n",
                page.id
            ));
        }
    }
    if !page.errors.is_empty() {
        text.push_str("\nerrors of the failed keys (--error N lists one group's keys):\n");
        for (i, group) in page.errors.iter().enumerate() {
            text.push_str(&format!(
                "  {i}  {}: {}\n",
                plural(group.keys as u64, "key"),
                group.error
            ));
        }
    }
    out(&mut rendered, text);
    if page.truncated {
        let shown = page.keys.len();
        rendered.chunks.push(OutputChunk::Stderr(format!(
            "showing {shown} of {} keys; {}\n",
            page.total,
            page.hint.as_deref().unwrap_or_default()
        )));
    }
    rendered
}

/// The grid's rows as text: a legend, then one strip of states per row, a long one wrapped
/// with the value each line starts at.
fn rows_text(along: &str, values: &[String], rows: &[(&str, Vec<char>)]) -> String {
    let span = match values.len() {
        0 => String::new(),
        n if n <= 12 => values.join(", "),
        n => format!("{} … {} ({n} values)", values[0], values[n - 1]),
    };
    let mut text = format!("each row runs along {along}: {span}\n");
    text.push_str("c fresh  p pinned  s stale  n never run  f failed  b blocked  - not a key\n\n");
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    for (label, states) in rows {
        if states.len() <= STRIP {
            let label = if label.is_empty() { along } else { label };
            let width = width.max(label.chars().count());
            text.push_str(&format!(
                "{label:<width$}  {}\n",
                states.iter().collect::<String>()
            ));
            continue;
        }
        // A long dimension wraps: each line starts at the value it names.
        if !label.is_empty() {
            text.push_str(&format!("{label}\n"));
        }
        let from = values
            .iter()
            .step_by(STRIP)
            .map(|v| v.chars().count())
            .max()
            .unwrap_or(0);
        for (line, cells) in states.chunks(STRIP).enumerate() {
            text.push_str(&format!(
                "  {:<from$}  {}\n",
                values[line * STRIP],
                cells.iter().collect::<String>()
            ));
        }
    }
    text
}

/// `barca status NODE --overview`: the node's keys in one picture
/// ([`PartitionGrid::overview`]): the counts, what is wrong and where, and the grid's rows.
/// JSON is the overview as `GET /assets/{name}/partitions/overview` answers it.
pub fn render_partition_overview(overview: &KeysOverview, json: bool) -> RenderedOutput {
    let mut rendered = RenderedOutput::default();
    if json {
        out(&mut rendered, pretty(overview));
        return rendered;
    }
    let mut text = counts_line(&overview.id, overview.known, &overview.counts);
    text.push('\n');
    if !overview.issues.is_empty() {
        text.push_str("\nwhat needs attention, the most keys first:\n");
        for issue in &overview.issues {
            let mut line = format!(
                "  {}  {}: {}",
                plural(issue.keys as u64, "key"),
                word(&issue.state),
                issue.title
            );
            if !issue.places.is_empty() {
                let places: Vec<String> = issue
                    .places
                    .iter()
                    .map(|p| format!("{} {}", p.dimension, p.words))
                    .collect();
                line.push_str(&format!(" ({})", places.join(" · ")));
            }
            text.push_str(&line);
            text.push('\n');
            let list = match issue.error {
                Some(error) => format!("--state {} --error {error}", issue.state),
                None => format!("--state {}", issue.state),
            };
            text.push_str(&format!(
                "    list: barca status {} --keys {list}\n",
                overview.id
            ));
            if let Some(selection) = &issue.selection {
                text.push_str(&format!(
                    "    run:  barca get {} --partition '{selection}'\n",
                    overview.id
                ));
            }
        }
    }
    if let Some(along) = &overview.along {
        let values = overview
            .dimensions
            .iter()
            .find(|d| &d.name == along)
            .map(|d| d.values.as_slice())
            .unwrap_or_default();
        let rows: Vec<(&str, Vec<char>)> = overview
            .rows
            .iter()
            .map(|r| (r.label.as_str(), r.states.chars().collect()))
            .collect();
        text.push('\n');
        text.push_str(&rows_text(along, values, &rows));
    }
    out(&mut rendered, text);
    if overview.truncated {
        rendered.chunks.push(OutputChunk::Stderr(format!(
            "showing {} of {} rows; {}\n",
            overview.rows.len(),
            overview.rows_total,
            overview.hint.as_deref().unwrap_or_default()
        )));
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
    let mut text = counts_line(&grid.id, grid.known, &grid.counts);
    text.push('\n');
    let mut blocks = grid.blocks();
    let total = blocks.len();
    if let Some(limit) = limit {
        blocks.truncate(limit);
    }
    let page = Page::new(blocks.len(), total);
    if let Some(along) = grid.dimensions.get(grid.along()) {
        let states: Vec<char> = grid.states.chars().collect();
        let rows: Vec<(&str, Vec<char>)> = blocks
            .iter()
            .map(|b| {
                (
                    b.label.as_str(),
                    b.cells
                        .iter()
                        .map(|c| states.get(*c).copied().unwrap_or(state::NONE))
                        .collect(),
                )
            })
            .collect();
        text.push_str(&rows_text(&along.name, &along.values, &rows));
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
    use crate::key_query::{KeyQuery, KeyQueryArgs};
    use crate::partition_grid::GridDimension;

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
    fn the_keys_json_is_the_page_the_route_serves_and_total_counts_what_matches() {
        let states = ["failed".to_string(), "never_run".to_string()];
        let query = KeyQuery::parse(&KeyQueryArgs {
            states: &states,
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
        let page = grid().keys_page(&query).unwrap();
        let text = stdout(render_partition_keys(&page, true, true));
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["total"], 2);
        assert_eq!(body["truncated"], true);
        assert_eq!(body["keys"].as_array().unwrap().len(), 1);
        assert_eq!(body["keys"][0]["key"], "day=d1,region=amer");
        assert_eq!(body["keys"][0]["values"], serde_json::json!(["d1", "amer"]));
        assert_eq!(body["counts"]["fresh"], 2);
        assert_eq!(body["dimensions"], serde_json::json!(["day", "region"]));
        assert_eq!(body["needs_run"], 2);
        assert!(body["hint"].as_str().unwrap().contains("--all"));
        let back: KeysPage = serde_json::from_str(&text).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&page).unwrap()
        );
    }

    #[test]
    fn the_keys_table_says_what_matches_how_to_run_it_and_numbers_the_errors() {
        let failed = ["failed".to_string()];
        let query = KeyQuery::parse(&KeyQueryArgs {
            states: &failed,
            ..Default::default()
        })
        .unwrap();
        let text = stdout(render_partition_keys(
            &grid().keys_page(&query).unwrap(),
            true,
            false,
        ));
        assert!(
            text.starts_with("p.py:events: 5 keys · 1 failed · 1 stale · 1 never run · 2 fresh\n")
        );
        assert!(text.contains("day=d1,region=amer  failed"), "{text}");
        assert!(text.contains("1 key match, 1 of them to run\n"), "{text}");
        assert!(
            text.contains(
                "run them: barca get p.py:events --partition '{\"day\":[\"d1\"],\"region\":[\"amer\"]}'"
            ),
            "{text}"
        );
        assert!(
            text.contains("  0  1 key: ValueError: no rows for <region>"),
            "{text}"
        );
        let pinned = ["pinned".to_string()];
        let query = KeyQuery::parse(&KeyQueryArgs {
            states: &pinned,
            ..Default::default()
        })
        .unwrap();
        let none = stdout(render_partition_keys(
            &grid().keys_page(&query).unwrap(),
            true,
            false,
        ));
        assert!(none.contains("no key matches\n"), "{none}");
    }

    #[test]
    fn the_overview_lists_each_issue_with_where_it_is_and_how_to_list_and_run_it() {
        let overview = grid().overview(None);
        let text = stdout(render_partition_overview(&overview, false));
        assert!(
            text.contains("what needs attention, the most keys first:\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "  1 key  failed: ValueError: no rows for <region> (day d1 · region amer)\n    \
                 list: barca status p.py:events --keys --state failed --error 0\n"
            ),
            "{text}"
        );
        assert!(text.contains("  1 key  never run: Never run"), "{text}");
        assert!(text.ends_with("day=d1  cfn\nday=d2  -cs\n"), "{text}");
        let json = stdout(render_partition_overview(&overview, true));
        let back: KeysOverview = serde_json::from_str(&json).unwrap();
        assert_eq!(back.rows.len(), 2);
        assert_eq!(back.along.as_deref(), Some("region"));
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
