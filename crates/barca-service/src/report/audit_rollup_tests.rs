//! `barca status --rollup` as text, over the audit shape (698 nodes, 422 of them not fresh):
//! the rollup `barca_analysis::causes`'s test `on_the_audit_shape_actionable_roots_lead`
//! computes and records, without its 698 node entries: the text goes down to groups and files
//! and prints no node's own row. The read model is below the renderer, so its test cannot print
//! it; this one reads the record and asserts the text.

#[test]
fn the_audit_shape_prints_its_to_do_list() {
    let recorded = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../barca-analysis/testdata/audit_rollup.json");
    let rollup: crate::rollup::Rollup =
        serde_json::from_str(&std::fs::read_to_string(recorded).unwrap()).unwrap();
    // The whole to-do list, as `barca status --rollup` prints it.
    let text: String = super::render_rollup(&rollup, false)
        .chunks
        .iter()
        .map(|chunk| match chunk {
            super::OutputChunk::Stdout(text) | super::OutputChunk::Stderr(text) => text.as_str(),
        })
        .collect();
    println!("{text}");
    assert!(text.contains("What holds the 422 nodes that are not fresh, and what clears it:"));
    assert!(text.contains("The first 3 hold 338 of them."));
    assert!(text.contains("1. no schedule: ledger"));
    assert!(
        text.contains("Distance from fresh: 1728 (weighted nodes); 698 when every node is fresh")
    );
    assert!(text.contains(
        "do:   add a schedule: freshness=Schedule(...) on it or on a node above it (about 7m of compute)"
    ));
    assert!(text.contains(
        "Gap: 152 no refresh path · 6 queued · 247 undecided · 17 catching up (nodes); \
         about 23 fresh within 4m, 399 with no estimate"
    ));
}
