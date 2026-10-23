//! The arguments of a dataset sink: `@sink("exports/scored/", partition_by=[...],
//! rows_per_file=N)`.
//!
//! A sink path ending in `/` is a dataset prefix. The worker writes it as a Hive-partitioned
//! dataset (`barca/_sinks.py`): one directory level per partition dimension of the asset, in
//! declared order, then one per `partition_by` column. What can be wrong with these arguments
//! is checked here, when the file is read (exit 2), so a typo never reaches a run.

use ruff_python_ast::{self as ast, Expr, Keyword};
use ruff_text_size::Ranged;

use crate::decorator_args::{BarcaNames, Problem};

const SEE: &str = "See `barca docs sinks`.";

/// The `partition_by=` columns and `rows_per_file=` of one `@sink(...)` call, as far as they
/// are literals. What is not a literal is reported by [`check`].
pub(crate) fn extract(call: &ast::ExprCall) -> (Vec<String>, Option<u64>) {
    let partition_by = keyword(call, "partition_by")
        .and_then(|kw| string_list(&kw.value))
        .unwrap_or_default();
    let rows_per_file = keyword(call, "rows_per_file").and_then(|kw| positive_int(&kw.value));
    (partition_by, rows_per_file)
}

/// The partition dimensions of `@asset(partitions={...})`, in the order they are written.
pub(crate) fn dimension_order(keywords: &[&Keyword]) -> Vec<String> {
    keywords
        .iter()
        .filter(|kw| kw.arg.as_ref().is_some_and(|a| a.as_str() == "partitions"))
        .filter_map(|kw| match &kw.value {
            Expr::Dict(dict) => Some(dict),
            _ => None,
        })
        .flat_map(|dict| dict.items.iter())
        .filter_map(|item| match item.key.as_ref() {
            Some(Expr::StringLiteral(s)) => Some(s.value.to_string()),
            _ => None,
        })
        .collect()
}

/// The first problem with the dataset arguments of the `@sink` decorators of a node whose
/// partition dimensions are `dims`.
pub(crate) fn check(
    decorators: &[ast::Decorator],
    barca: &BarcaNames,
    dims: &[String],
) -> Option<Problem> {
    decorators
        .iter()
        .find_map(|decorator| match &decorator.expression {
            Expr::Call(call) if barca.recognized(&call.func) == Some("sink") => {
                check_call(call, dims)
            }
            _ => None,
        })
}

fn check_call(call: &ast::ExprCall, dims: &[String]) -> Option<Problem> {
    let Some(Expr::StringLiteral(path)) = call.arguments.args.first() else {
        return None;
    };
    let path = path.value.to_string();
    let dataset = path.ends_with('/');
    let serializer = keyword(call, "serializer").and_then(|kw| match &kw.value {
        Expr::StringLiteral(s) => Some(s.value.to_string().to_lowercase()),
        _ => None,
    });
    let problem = |at: &dyn Ranged, message: String, fix: String| {
        Some(Problem {
            offset: at.range().start().to_usize(),
            message,
            fix: format!("{fix} {SEE}"),
        })
    };

    if dataset
        && let Some(kw) = keyword(call, "serializer")
        && let Some(fmt) = serializer.as_deref()
        && !matches!(fmt, "parquet" | "csv" | "json")
    {
        return problem(
            kw,
            format!(
                "the dataset sink `{path}` cannot be written as {fmt}: a dataset sink (a path \
                 ending in `/`) writes parquet, csv or json"
            ),
            "Set serializer= to \"parquet\" (the default for tables), \"csv\" or \"json\", or \
             drop the trailing `/` to write one file."
                .to_string(),
        );
    }

    for name in ["partition_by", "rows_per_file"] {
        if !dataset && let Some(kw) = keyword(call, name) {
            return problem(
                kw,
                format!(
                    "`{name}=` applies to a dataset sink, a path ending in `/`; `{path}` is a \
                     file sink"
                ),
                format!(
                    "End the path with `/` to write a partitioned dataset (`@sink(\"{}/\", \
                     {name}=...)`), or remove `{name}=`.",
                    path.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&path)
                ),
            );
        }
    }

    if let Some(kw) = keyword(call, "partition_by") {
        let Some(columns) = string_list(&kw.value) else {
            return problem(
                kw,
                "`partition_by=` must be a list of column names written out as string \
                 literals"
                    .to_string(),
                "Write it like `partition_by=[\"order_date\"]`.".to_string(),
            );
        };
        if columns.is_empty() {
            return problem(
                kw,
                "`partition_by=[]` names no column".to_string(),
                "Name the columns to partition by, like `partition_by=[\"order_date\"]`, or \
                 remove `partition_by=`."
                    .to_string(),
            );
        }
        for (i, column) in columns.iter().enumerate() {
            if column.is_empty() || column.contains(['/', '=']) {
                return problem(
                    kw,
                    format!(
                        "`partition_by=` names the column {column:?}, which cannot be a \
                         directory level (an empty name, or one with `/` or `=`)"
                    ),
                    "Partition by a column whose name has no `/` or `=`.".to_string(),
                );
            }
            if columns[..i].contains(column) {
                return problem(
                    kw,
                    format!("`partition_by=` names the column `{column}` twice"),
                    format!("Name `{column}` once."),
                );
            }
            if dims.contains(column) {
                return problem(
                    kw,
                    format!(
                        "`partition_by=` names `{column}`, which is already a partition \
                         dimension of the asset: its directory level comes first, from the \
                         asset's key"
                    ),
                    format!("Remove `{column}` from `partition_by=`."),
                );
            }
        }
    }

    if let Some(kw) = keyword(call, "rows_per_file") {
        if positive_int(&kw.value).is_none() {
            return problem(
                kw,
                "`rows_per_file=` must be a whole number of at least 1, written as a literal"
                    .to_string(),
                "Write it like `rows_per_file=1_000_000`.".to_string(),
            );
        }
        if let Some(fmt @ ("csv" | "json")) = serializer.as_deref() {
            return problem(
                kw,
                format!(
                    "`rows_per_file=` applies to parquet; a {fmt} dataset sink writes one file \
                     per partition"
                ),
                "Remove `rows_per_file=`, or write parquet.".to_string(),
            );
        }
    }
    None
}

fn keyword<'a>(call: &'a ast::ExprCall, name: &str) -> Option<&'a Keyword> {
    call.arguments
        .keywords
        .iter()
        .find(|kw| kw.arg.as_ref().is_some_and(|a| a.as_str() == name))
}

/// `["a", "b"]` or `("a", "b")`: every element a string literal.
fn string_list(expr: &Expr) -> Option<Vec<String>> {
    let elts = match expr {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    elts.iter()
        .map(|e| match e {
            Expr::StringLiteral(s) => Some(s.value.to_string()),
            _ => None,
        })
        .collect()
}

fn positive_int(expr: &Expr) -> Option<u64> {
    match expr {
        Expr::NumberLiteral(n) => match &n.value {
            ast::Number::Int(i) => i.as_u64().filter(|v| *v >= 1),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::model::SinkDecl;

    fn parse(decorators: &str) -> Result<Vec<SinkDecl>, String> {
        let source = format!(
            "from barca import asset, sink, partitions\n\n{decorators}\ndef scored(retailer=None, region=None):\n    \
             return 1\n"
        );
        crate::parse::extract_nodes(&source, "pipeline.py")
            .map(|nodes| nodes[0].sinks.to_vec())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_dataset_sink_reads_its_arguments_and_the_asset_dimensions_in_order() {
        let sinks = parse(
            "@asset(partitions={\"retailer\": partitions([\"r1\"]), \"region\": \
             partitions([\"eu\"])})\n@sink(\"s3://b/scored/\", partition_by=[\"order_date\"], \
             rows_per_file=1000)",
        )
        .unwrap();
        assert_eq!(sinks[0].path, "s3://b/scored/");
        assert!(sinks[0].is_dataset());
        assert_eq!(sinks[0].partition_by, ["order_date"]);
        assert_eq!(sinks[0].rows_per_file, Some(1000));
        assert_eq!(sinks[0].dims, ["retailer", "region"]);
    }

    #[test]
    fn a_file_sink_is_unchanged() {
        let sinks = parse(
            "@asset(partitions={\"retailer\": partitions([\"r1\"])})\n@sink(\"out.parquet\")",
        )
        .unwrap();
        assert!(!sinks[0].is_dataset());
        assert!(sinks[0].dims.is_empty());
        // What the worker is sent for a file sink is what it was before dataset sinks.
        assert_eq!(
            serde_json::to_value(&sinks[0]).unwrap(),
            serde_json::json!({"path": "out.parquet", "serializer": null})
        );
    }

    #[test]
    fn dataset_arguments_on_a_file_sink_are_an_error() {
        for args in ["partition_by=[\"d\"]", "rows_per_file=10"] {
            let err = parse(&format!("@asset()\n@sink(\"out.parquet\", {args})")).unwrap_err();
            assert!(err.contains("applies to a dataset sink"), "{err}");
            assert!(err.contains("@sink(\"out/\""), "{err}");
        }
    }

    #[test]
    fn bad_dataset_arguments_are_errors() {
        let cases = [
            ("partition_by=\"d\"", "list of column names"),
            ("partition_by=[COL]", "list of column names"),
            ("partition_by=[]", "names no column"),
            ("partition_by=[\"d\", \"d\"]", "twice"),
            ("partition_by=[\"a/b\"]", "cannot be a directory level"),
            (
                "partition_by=[\"retailer\"]",
                "already a partition dimension",
            ),
            ("rows_per_file=0", "at least 1"),
            ("rows_per_file=1.5", "at least 1"),
            ("rows_per_file=N", "at least 1"),
            (
                "serializer=\"csv\", rows_per_file=10",
                "one file per partition",
            ),
            ("serializer=\"pickle\"", "writes parquet, csv or json"),
        ];
        for (args, expected) in cases {
            let err = parse(&format!(
                "@asset(partitions={{\"retailer\": partitions([\"r1\"])}})\n@sink(\"out/\", \
                 {args})"
            ))
            .unwrap_err();
            assert!(err.contains(expected), "{args}: {err}");
            assert!(err.contains("barca docs sinks"), "{args}: {err}");
        }
    }

    #[test]
    fn good_dataset_sinks_parse() {
        for args in [
            "",
            "partition_by=(\"d\",)",
            "serializer=\"csv\", partition_by=[\"d\"]",
            "serializer=\"json\"",
            "rows_per_file=1_000_000",
        ] {
            let sep = if args.is_empty() { "" } else { ", " };
            parse(&format!("@asset()\n@sink(\"out/\"{sep}{args})")).unwrap();
        }
    }
}
