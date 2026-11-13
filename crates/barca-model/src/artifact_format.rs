//! An artifact file's format, read from its extension: the one table of
//! specs/interfaces/artifact-formats.md ("Artifact file extensions"), which the Python worker
//! holds as `barca._artifacts.FORMAT_OF_EXTENSION`. `barca sql` and the server
//! (`GET /runs/{run_id}/output`) read it from here, so the two cannot drift apart again
//! (the server once read every unknown extension as pickle, `barca sql` as `unknown`).

use std::path::Path;

/// The extension table: `(extension without the dot, format)`, in the spec's order.
pub const FORMAT_OF_EXTENSION: &[(&str, &str)] = &[
    ("json", "json"),
    ("pkl", "pickle"),
    ("pickle", "pickle"),
    ("parquet", "parquet"),
    ("csv", "csv"),
];

/// The format of a file whose extension is not in the table: not an artifact format. The
/// worker reads such a file as JSON; `barca sql` reports it as `unknown`.
pub const UNKNOWN: &str = "unknown";

/// An artifact's format from its file name (`path` may be a local path or a URI).
pub fn format_of(path: &str) -> &'static str {
    let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) else {
        return UNKNOWN;
    };
    FORMAT_OF_EXTENSION
        .iter()
        .find(|(known, _)| *known == ext)
        .map_or(UNKNOWN, |(_, format)| format)
}

/// The `Content-Type` an artifact of `format` is served with. Pickle and anything unknown are
/// bytes.
pub fn content_type(format: &str) -> &'static str {
    match format {
        "json" => "application/json",
        "parquet" => "application/vnd.apache.parquet",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_ast::{Expr, Stmt};

    #[test]
    fn formats_come_from_the_extension() {
        assert_eq!(format_of(".barca/artifacts/a/h.parquet"), "parquet");
        assert_eq!(format_of("s3://bucket/a/h.json"), "json");
        assert_eq!(format_of("x.pkl"), "pickle");
        assert_eq!(format_of("x.pickle"), "pickle");
        assert_eq!(format_of("x.csv"), "csv");
        assert_eq!(format_of("x"), UNKNOWN);
        assert_eq!(format_of("x.jsonl"), UNKNOWN);
        assert_eq!(
            format_of("x.PKL"),
            UNKNOWN,
            "extensions are case-sensitive, as in Python"
        );
    }

    #[test]
    fn every_format_has_a_content_type_and_bytes_are_the_default() {
        assert_eq!(content_type("json"), "application/json");
        assert_eq!(content_type("parquet"), "application/vnd.apache.parquet");
        assert_eq!(content_type("csv"), "text/csv");
        assert_eq!(content_type("pickle"), "application/octet-stream");
        assert_eq!(content_type(UNKNOWN), "application/octet-stream");
    }

    /// The Python worker's `FORMAT_OF_EXTENSION` (python/barca/_artifacts.py), read from the
    /// source as a literal dict, is this table, entry for entry.
    #[test]
    fn the_table_is_the_python_workers_table() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/barca/_artifacts.py");
        let source = std::fs::read_to_string(&path).unwrap();
        let module = ruff_python_parser::parse_module(&source)
            .unwrap()
            .into_syntax();
        let dict = module
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::Assign(assign)
                    if assign.targets.iter().any(|t| {
                        t.as_name_expr()
                            .is_some_and(|n| n.id == "FORMAT_OF_EXTENSION")
                    }) =>
                {
                    match &*assign.value {
                        Expr::Dict(dict) => Some(dict),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("python/barca/_artifacts.py assigns FORMAT_OF_EXTENSION a literal dict");
        let python: Vec<(String, String)> = dict
            .items
            .iter()
            .map(|item| {
                let text = |e: &Expr| match e {
                    Expr::StringLiteral(s) => s.value.to_str().to_string(),
                    other => panic!("FORMAT_OF_EXTENSION holds only string literals: {other:?}"),
                };
                let ext = text(item.key.as_ref().expect("no ** in FORMAT_OF_EXTENSION"));
                let ext = ext
                    .strip_prefix('.')
                    .unwrap_or_else(|| panic!("Python keys start with a dot: {ext}"))
                    .to_string();
                (ext, text(&item.value))
            })
            .collect();
        let rust: Vec<(String, String)> = FORMAT_OF_EXTENSION
            .iter()
            .map(|(e, f)| (e.to_string(), f.to_string()))
            .collect();
        assert_eq!(
            rust, python,
            "crates/barca-model/src/artifact_format.rs and python/barca/_artifacts.py differ"
        );
    }
}
