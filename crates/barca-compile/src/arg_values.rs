//! Reading one value written in a decorator: the small readers and the wording of "barca cannot
//! read this" that the parser ([`crate::parse`]) and the per-decorator checks
//! ([`crate::sink_args`], [`crate::decorators`]) share. A leaf: syntax in, values or a
//! [`Rejected`] out.

use ruff_python_ast::{Expr, Keyword};
use ruff_text_size::Ranged;

use barca_model::model::SerializerKind;

/// A decorator value barca cannot read: the byte offset of the value, what is wrong, and what to
/// write instead.
pub(crate) type Rejected = (usize, String, String);

/// How barca reads decorator arguments, for a message about one it cannot read.
const NOT_RUN: &str = "barca reads decorator arguments from the source without running it, so it \
                       cannot see what this is";

/// The source text of `expr`, shortened to its first line and 60 characters.
pub(crate) fn snippet(source: &str, expr: &Expr) -> String {
    let range = expr.range();
    let text = source
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or("?");
    let first = text.lines().next().unwrap_or("");
    if first.chars().count() > 60 {
        format!("{}...", first.chars().take(57).collect::<String>())
    } else if first.len() < text.len() {
        format!("{first}...")
    } else {
        first.to_string()
    }
}

/// "; barca reads ... without running it" after a value that is computed (a name, a call, an
/// operator): nothing for a literal of the wrong kind, which barca reads well enough.
pub(crate) fn not_run(expr: &Expr) -> String {
    let literal = match expr {
        Expr::UnaryOp(op) => matches!(op.operand.as_ref(), Expr::NumberLiteral(_)),
        Expr::List(_) | Expr::Tuple(_) | Expr::Set(_) | Expr::Dict(_) => true,
        e => e.is_literal_expr(),
    };
    if literal {
        String::new()
    } else {
        format!("; {NOT_RUN}")
    }
}

/// `arg=` holds `expr`, which is not `wants`.
pub(crate) fn unreadable(
    source: &str,
    expr: &Expr,
    arg: &str,
    wants: &str,
    fix: String,
) -> Rejected {
    (
        expr.range().start().to_usize(),
        format!(
            "`{arg}=` must be {wants}, got `{}`{}",
            snippet(source, expr),
            not_run(expr)
        ),
        fix,
    )
}

/// The keyword `name=` of a decorator, when it is written.
pub(crate) fn keyword<'a>(keywords: &[&'a Keyword], name: &str) -> Option<&'a Keyword> {
    keywords
        .iter()
        .find(|kw| kw.arg.as_ref().is_some_and(|a| a.as_str() == name))
        .copied()
}

/// The serializers `serializer=` names, matched without regard to case.
fn serializer_named(name: &str) -> Option<SerializerKind> {
    match name.to_lowercase().as_str() {
        "json" => Some(SerializerKind::Json),
        "parquet" => Some(SerializerKind::Parquet),
        "csv" => Some(SerializerKind::Csv),
        "pickle" => Some(SerializerKind::Pickle),
        "text" => Some(SerializerKind::Text),
        "yaml" => Some(SerializerKind::Yaml),
        _ => None,
    }
}

/// `serializer="json"` (or `None`, the default) on a node or a sink (#665): a string literal
/// naming a serializer. A name, a call or an unknown format is refused rather than read as
/// "not written", which would store the value in a format nobody asked for.
pub(crate) fn serializer_kind(
    expr: &Expr,
    source: &str,
    topic: &str,
) -> Result<Option<SerializerKind>, Rejected> {
    let fix = format!(
        "Write it like `serializer=\"json\"`: one of \"json\", \"pickle\", \"parquet\" or \
         \"csv\". See `barca docs {topic}`."
    );
    match expr {
        Expr::NoneLiteral(_) => Ok(None),
        Expr::StringLiteral(s) => match serializer_named(s.value.to_str()) {
            Some(kind) => Ok(Some(kind)),
            None => Err((
                expr.range().start().to_usize(),
                format!(
                    "`serializer=` names no serializer barca has: {:?}",
                    s.value.to_str()
                ),
                fix,
            )),
        },
        _ => Err(unreadable(
            source,
            expr,
            "serializer",
            "a string literal",
            fix,
        )),
    }
}

pub(crate) fn extract_string_literal(expr: &Expr) -> Option<String> {
    if let Expr::StringLiteral(s) = expr {
        Some(s.value.to_string())
    } else {
        None
    }
}
