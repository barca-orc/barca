//! Which barca decorator an expression is, read from the decorator alone: `@asset` / `@sensor` /
//! `@task`, `@unsafe`, `@sink(...)` and `cutoff=True`. The parser ([`crate::parse`]) extracts
//! nodes with these; the definition fingerprint ([`crate::definition`]) and the dependency cone
//! ([`crate::cone`]) recognize decorators with them before any node exists.

use ruff_python_ast::{self as ast, Expr, Keyword};
use ruff_text_size::Ranged;

use crate::arg_values::{extract_string_literal, serializer_kind};
use crate::decorator_args::BarcaNames;
use barca_model::model::{NodeKind, SinkDecl};

pub(crate) fn is_unsafe_decorator(expr: &Expr, barca: &BarcaNames) -> bool {
    barca.recognized(expr) == Some("unsafe")
}

pub(crate) fn try_extract_sink(expr: &Expr, barca: &BarcaNames) -> Option<SinkDecl> {
    if let Expr::Call(call) = expr
        && barca.recognized(&call.func) == Some("sink")
    {
        let path = call
            .arguments
            .args
            .first()
            .and_then(extract_string_literal)?;
        let serializer = call
            .arguments
            .keywords
            .iter()
            .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("serializer"))
            // An unreadable serializer is reported by `sink_args::check`.
            .and_then(|kw| serializer_kind(&kw.value, "", "sinks").ok().flatten());
        let (partition_by, rows_per_file) = crate::sink_args::extract(call);
        return Some(SinkDecl {
            path,
            serializer,
            partition_by,
            rows_per_file,
            dims: Vec::new(),
        });
    }
    None
}

pub(crate) fn match_node_decorator<'a>(
    expr: &'a Expr,
    barca: &BarcaNames,
) -> Option<(NodeKind, Vec<&'a Keyword>)> {
    let (callee, keywords) = match expr {
        Expr::Call(call) => (call.func.as_ref(), call.arguments.keywords.iter().collect()),
        _ => (expr, Vec::new()),
    };
    let kind = match barca.recognized(callee)? {
        "asset" => NodeKind::Asset,
        "sensor" => NodeKind::Sensor,
        "task" => NodeKind::Task,
        _ => return None,
    };
    Some((kind, keywords))
}

/// Whether `func` is an `@asset(cutoff=True)` (#628), read from its decorators alone: for the
/// dependency cone (`crate::cone`, rule 8), which sees functions before nodes are extracted.
pub(crate) fn is_cutoff_asset(func: &ast::StmtFunctionDef, barca: &BarcaNames) -> bool {
    func.decorator_list.iter().any(|d| {
        match_node_decorator(&d.expression, barca).is_some_and(|(kind, keywords)| {
            kind == NodeKind::Asset && extract_cutoff(&keywords, "").unwrap_or(false)
        })
    })
}

pub(crate) fn extract_cutoff(keywords: &[&Keyword], source: &str) -> Result<bool, (usize, String)> {
    let Some(kw) = keywords
        .iter()
        .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("cutoff"))
    else {
        return Ok(false);
    };
    if let Expr::BooleanLiteral(b) = &kw.value {
        return Ok(b.value);
    }
    let range = kw.value.range();
    Err((
        range.start().to_usize(),
        format!(
            "cutoff= must be True or False, got `{}`",
            source
                .get(range.start().to_usize()..range.end().to_usize())
                .unwrap_or("?")
        ),
    ))
}
