//! What a node's definition hash covers.
//!
//! The definition hash of an `@asset`, `@sensor` or `@task` function covers what determines its
//! result, and nothing else:
//!
//! 1. the function from `def` (or `async def`) to its end, as written;
//! 2. the parts of its decorators listed as counted in [`RULES`], in a canonical form;
//! 3. its dependency cone ([`crate::cone`]): the helper code the body reaches, and the helper
//!    code the counted decorator parts reach.
//!
//! [`RULES`] is the one list of what counts. `barca docs cache` ("Which decorator arguments
//! count") mirrors it row for row, and a test fails when the two differ.
//!
//! **Canonical, not textual.** A counted part is written from the syntax tree, never sliced from
//! the source, so whitespace, comments, quote style, trailing commas, redundant parentheses and
//! the order of keyword arguments do not change the hash ([`canonical_expr`]). The function from
//! `def` on is hashed as written, as it always has been.
//!
//! **Narrowing.** Each entry of [`RULES`] is independent: to make an argument count, change its
//! `counts` to `Counts::Yes`; to stop one counting, `Counts::No`; and change the same row of
//! the table in `cache.md`. No other code has to change. Either way every definition hash that
//! the argument appears in changes once.

use ruff_python_ast::{self as ast, Expr, Number};
use ruff_text_size::Ranged;

use crate::parse::{is_unsafe_decorator, match_node_decorator, try_extract_sink};

/// Whether a part of a node's decorators is part of its definition hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counts {
    /// The canonical form of the value is hashed, and the cone follows the names in it.
    Yes,
    /// Not hashed and not followed.
    No,
    /// `partitions=`: the dimension names and, for `partitions_from`, the source are hashed. The
    /// keys are not (see the reason on the rule).
    PartitionShape,
}

/// One row of the list: a part of a node's decorators, whether it counts, and why.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    /// The keyword argument of `@asset` / `@sensor` / `@task`, or one of the names below for a
    /// part that is not a keyword argument.
    pub part: &'static str,
    pub counts: Counts,
    pub reason: &'static str,
}

/// A stacked `@sink(...)` decorator.
pub const SINK: &str = "@sink(...)";
/// A decorator that is not one of barca's (`@functools.cache`, a wrapper of your own).
pub const OTHER_DECORATOR: &str = "any other decorator";
/// A keyword argument of `@asset` / `@sensor` / `@task` that is not in the list, a positional
/// argument, or a `**mapping`.
pub const UNKNOWN_ARGUMENT: &str = "any other argument";
/// The `@unsafe` marker.
pub const UNSAFE: &str = "@unsafe";

/// The list of what counts. Mirrored exactly in `crates/barca-cli/docs/cache.md`.
pub const RULES: &[Rule] = &[
    Rule {
        part: "inputs",
        counts: Counts::Yes,
        reason: "decides which upstream feeds which parameter",
    },
    Rule {
        part: "serializer",
        counts: Counts::Yes,
        reason: "changes the bytes that are stored",
    },
    Rule {
        part: "partitions",
        counts: Counts::PartitionShape,
        reason: "the dimension names and a `partitions_from` source decide how the function is \
                 called; the keys do not count, because each key is part of its own run hash",
    },
    Rule {
        part: SINK,
        counts: Counts::Yes,
        reason: "a cached step does not write its sinks, so a new or edited sink has to run the \
                 asset to be written",
    },
    Rule {
        part: OTHER_DECORATOR,
        counts: Counts::Yes,
        reason: "it wraps the function and can change what it returns",
    },
    Rule {
        part: UNKNOWN_ARGUMENT,
        counts: Counts::Yes,
        reason: "conservative fallback for unvalidated calls; recognized barca calls reject it",
    },
    Rule {
        part: "env",
        counts: Counts::No,
        reason: "the declared names and their values are already part of the run hash",
    },
    Rule {
        part: "name",
        counts: Counts::No,
        reason: "it is the node's id, under which results are looked up, not part of the result",
    },
    Rule {
        part: "freshness",
        counts: Counts::No,
        reason: "decides when the step runs, not what it returns",
    },
    Rule {
        part: "retries",
        counts: Counts::No,
        reason: "decides how often a failing step is tried, not what it returns",
    },
    Rule {
        part: "retry_backoff",
        counts: Counts::No,
        reason: "decides how long to wait between attempts, not what the step returns",
    },
    Rule {
        part: "timeout_seconds",
        counts: Counts::No,
        reason: "decides how long the step may take, not what it returns",
    },
    Rule {
        part: "description",
        counts: Counts::No,
        reason: "documentation",
    },
    Rule {
        part: "tags",
        counts: Counts::No,
        reason: "labels for people and tools",
    },
    Rule {
        part: UNSAFE,
        counts: Counts::No,
        reason: "a marker barca reads and does not act on",
    },
];

/// The rule for `part`; a keyword argument that is not listed follows [`UNKNOWN_ARGUMENT`].
fn rule(part: &str) -> Counts {
    let find = |part: &str| RULES.iter().find(|r| r.part == part).map(|r| r.counts);
    find(part)
        .or_else(|| find(UNKNOWN_ARGUMENT))
        .unwrap_or(Counts::Yes)
}

/// Hash rules keyed to the accepted node arguments, rather than a second signature table.
/// Unknown arguments can reach the conservative fallback only for calls whose barca binding
/// cannot be established; the parser validates positively bound barca calls first.
pub fn node_argument_rules()
-> impl Iterator<Item = (crate::decorator_args::Argument, &'static Rule)> {
    crate::decorator_args::arguments()
        .filter(|argument| matches!(argument.call, "asset" | "sensor" | "task"))
        .map(|argument| {
            let rule = RULES
                .iter()
                .find(|rule| rule.part == argument.name)
                .expect("every accepted node argument must have an explicit hash rule");
            (argument, rule)
        })
}

fn node_argument_counts(kind: crate::NodeKind, name: &str) -> Counts {
    let call = match kind {
        crate::NodeKind::Asset => "asset",
        crate::NodeKind::Sensor => "sensor",
        crate::NodeKind::Task => "task",
    };
    node_argument_rules()
        .find(|(argument, _)| argument.call == call && argument.name == name)
        .map(|(_, rule)| rule.counts)
        .unwrap_or_else(|| rule(UNKNOWN_ARGUMENT))
}

/// The hashed form of a node function.
pub struct Definition<'a> {
    /// The canonical counted decorator parts, one per line, then the source from `def` on.
    pub text: String,
    /// The expressions inside counted parts whose names the cone follows, like names in the
    /// body. A reference to an upstream node (`inputs={"x": up}`, `collect(up)`,
    /// `partitions_from(up)`) is not among them: the upstream's run hash, which covers its code,
    /// is already part of this node's run hash, and following it would add that code a second
    /// time.
    pub followed: Vec<&'a Expr>,
}

/// The definition of `func` if it is a node (has an `@asset`, `@sensor` or `@task` decorator),
/// else `None`. `source` is the text `func` was parsed from. Only names positively bound
/// to barca imports receive barca metadata rules; other decorators count and are followed
/// in full, including wrappers that happen to be named `asset`, `sensor` or `task`.
pub fn node_definition<'a>(
    func: &'a ast::StmtFunctionDef,
    source: &str,
    barca: &crate::decorator_args::BarcaNames,
) -> Option<Definition<'a>> {
    let mut is_node = false;
    // Decorators that count, in the order they are stacked.
    let mut decorators: Vec<String> = Vec::new();
    // Counted arguments of the node decorator, sorted before they are written.
    let mut arguments: Vec<String> = Vec::new();
    let mut followed: Vec<&'a Expr> = Vec::new();

    for decorator in &func.decorator_list {
        let expr = &decorator.expression;
        let name = match expr {
            Expr::Name(name) => Some(name.id.as_str()),
            Expr::Call(call) => match call.func.as_ref() {
                Expr::Name(name) => Some(name.id.as_str()),
                _ => None,
            },
            _ => None,
        };
        if !name.is_some_and(|name| barca.contains(name)) {
            is_node |= match_node_decorator(expr).is_some();
            decorators.push(canonical_expr(expr, source));
            followed.push(expr);
        } else if is_unsafe_decorator(expr) {
            if rule(UNSAFE) != Counts::No {
                decorators.push("unsafe".to_string());
            }
        } else if try_extract_sink(expr).is_some() {
            if rule(SINK) != Counts::No {
                decorators.push(canonical_expr(expr, source));
                // The arguments, not the name `sink` itself (it is barca's).
                if let Expr::Call(call) = expr {
                    followed.extend(call.arguments.args.iter());
                    followed.extend(call.arguments.keywords.iter().map(|kw| &kw.value));
                }
            }
        } else if let Some((kind, _)) = match_node_decorator(expr) {
            is_node = true;
            if let Expr::Call(call) = expr {
                node_arguments(kind, call, source, &mut arguments, &mut followed);
            }
        } else if rule(OTHER_DECORATOR) != Counts::No {
            decorators.push(canonical_expr(expr, source));
            followed.push(expr);
        }
    }
    if !is_node {
        return None;
    }

    arguments.sort();
    let mut text = String::new();
    for decorator in &decorators {
        text.push_str("decorator ");
        text.push_str(decorator);
        text.push('\n');
    }
    for argument in &arguments {
        text.push_str("argument ");
        text.push_str(argument);
        text.push('\n');
    }
    text.push_str(&source[def_start(func, source)..func.range().end().to_usize()]);
    Some(Definition { text, followed })
}

/// The counted arguments of one `@asset(...)` / `@sensor(...)` / `@task(...)` call.
fn node_arguments<'a>(
    kind: crate::NodeKind,
    call: &'a ast::ExprCall,
    source: &str,
    arguments: &mut Vec<String>,
    followed: &mut Vec<&'a Expr>,
) {
    for (position, arg) in call.arguments.args.iter().enumerate() {
        if rule(UNKNOWN_ARGUMENT) != Counts::No {
            arguments.push(format!("#{position}={}", canonical_expr(arg, source)));
            followed.push(arg);
        }
    }
    for keyword in &call.arguments.keywords {
        let value = &keyword.value;
        let Some(name) = keyword.arg.as_ref().map(|a| a.as_str()) else {
            // `**mapping`: what it holds cannot be read statically.
            if rule(UNKNOWN_ARGUMENT) != Counts::No {
                arguments.push(format!("**{}", canonical_expr(value, source)));
                followed.push(value);
            }
            continue;
        };
        match node_argument_counts(kind, name) {
            Counts::No => {}
            Counts::PartitionShape => {
                arguments.push(format!(
                    "{name}={}",
                    partition_shape(value, source, followed)
                ));
            }
            Counts::Yes if name == "inputs" => {
                arguments.push(format!("{name}={}", inputs(value, source, followed)));
            }
            Counts::Yes => {
                arguments.push(format!("{name}={}", canonical_expr(value, source)));
                followed.push(value);
            }
        }
    }
}

/// `inputs={"param": upstream, ...}`: the mapping, sorted by parameter (its order means
/// nothing). An upstream reference is hashed as written and not followed.
fn inputs<'a>(value: &'a Expr, source: &str, followed: &mut Vec<&'a Expr>) -> String {
    let Expr::Dict(dict) = value else {
        // Not a literal mapping: barca reads no inputs from it. Hashed and followed whole.
        followed.push(value);
        return canonical_expr(value, source);
    };
    let mut items: Vec<String> = Vec::with_capacity(dict.items.len());
    for item in &dict.items {
        let key = match &item.key {
            Some(key) => {
                if !key.is_string_literal_expr() {
                    followed.push(key);
                }
                canonical_expr(key, source)
            }
            None => "**".to_string(),
        };
        follow_unless_node_reference(&item.value, followed);
        items.push(format!("{key}: {}", canonical_expr(&item.value, source)));
    }
    items.sort();
    format!("{{{}}}", items.join(", "))
}

/// `partitions={"dim": partitions(...) | partitions_from(upstream)}`: for each dimension, its
/// name and either `keys` (whatever the keys are and however they are written) or the
/// `partitions_from` source. Sorted by dimension.
fn partition_shape<'a>(value: &'a Expr, source: &str, followed: &mut Vec<&'a Expr>) -> String {
    let Expr::Dict(dict) = value else {
        // Not a literal mapping: barca reads no partitions from it. Hashed and followed whole.
        followed.push(value);
        return canonical_expr(value, source);
    };
    let mut items: Vec<String> = Vec::with_capacity(dict.items.len());
    for item in &dict.items {
        let key = match &item.key {
            Some(key) => {
                if !key.is_string_literal_expr() {
                    followed.push(key);
                }
                canonical_expr(key, source)
            }
            None => "**".to_string(),
        };
        let shape = match barca_call(&item.value) {
            Some(("partitions", _)) => "keys".to_string(),
            Some(("partitions_from", call)) => {
                follow_unless_node_reference(&item.value, followed);
                format!("from {}", canonical_arguments(&call.arguments, source))
            }
            _ => {
                // Not a form barca reads as a dimension. Hashed and followed whole.
                followed.push(&item.value);
                canonical_expr(&item.value, source)
            }
        };
        items.push(format!("{key}: {shape}"));
    }
    items.sort();
    format!("{{{}}}", items.join(", "))
}

/// `name(...)` where `name` is a plain name: the name and the call.
fn barca_call(expr: &Expr) -> Option<(&str, &ast::ExprCall)> {
    let Expr::Call(call) = expr else {
        return None;
    };
    let Expr::Name(name) = call.func.as_ref() else {
        return None;
    };
    Some((name.id.as_str(), call))
}

/// Add `expr` to `followed`, leaving out what [`crate::parse`] reads as a reference to another
/// node: a name, `module.name`, and the callee and first argument of `collect(up)`,
/// `partitions_from(up)` and `up()`. `asset_ref("file.py:name")` names a node by a string.
fn follow_unless_node_reference<'a>(expr: &'a Expr, followed: &mut Vec<&'a Expr>) {
    let is_reference = |e: &Expr| matches!(e, Expr::Name(_) | Expr::Attribute(_));
    if is_reference(expr) {
        return;
    }
    let Some((_, call)) = barca_call(expr) else {
        followed.push(expr);
        return;
    };
    for (position, arg) in call.arguments.args.iter().enumerate() {
        if position > 0 || !is_reference(arg) {
            followed.push(arg);
        }
    }
    followed.extend(call.arguments.keywords.iter().map(|kw| &kw.value));
}

/// Where the function starts once its decorators are left out: at `def`, or at `async` for an
/// `async def`.
fn def_start(func: &ast::StmtFunctionDef, source: &str) -> usize {
    let name_start = func.name.range().start().to_usize();
    let before = |end: usize, word: &str| -> Option<usize> {
        let head = source[..end].trim_end_matches([' ', '\t', '\x0c', '\\', '\r', '\n']);
        let start = head.strip_suffix(word)?.len();
        // `word` must be a whole word, not the tail of a longer one.
        let boundary = source[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        boundary.then_some(start)
    };
    let Some(def) = before(name_start, "def") else {
        return name_start;
    };
    before(def, "async").unwrap_or(def)
}

// ─── Canonical form ──────────────────────────────────────────────────────────

/// `expr` written from the syntax tree in one fixed form.
///
/// Two expressions that differ only in whitespace, comments, line breaks, quote style, string
/// prefix, implicit string concatenation, trailing commas, redundant parentheses, the spelling
/// of a number (`0x10` and `16`) give the same text. Call keyword order is preserved: Python
/// decorators and helper calls can observe it through `**kwargs`.
/// Operators are fully parenthesised, so the text never depends on precedence.
///
/// f-strings, t-strings and lambdas are the exception: they are copied from `source` as
/// written.
pub fn canonical_expr(expr: &Expr, source: &str) -> String {
    let mut out = String::new();
    write_expr(expr, source, &mut out);
    out
}

fn canonical_arguments(arguments: &ast::Arguments, source: &str) -> String {
    let mut out = String::new();
    write_arguments(arguments, source, &mut out);
    out
}

fn write_list<'e>(
    exprs: impl IntoIterator<Item = &'e Expr>,
    open: &str,
    close: &str,
    source: &str,
    out: &mut String,
) {
    out.push_str(open);
    for (i, expr) in exprs.into_iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_expr(expr, source, out);
    }
    out.push_str(close);
}

/// Positional arguments, then keyword arguments and `**mapping` in their original order.
/// Sorting arbitrary calls would hide result changes in decorators that inspect `**kwargs`.
fn write_arguments(arguments: &ast::Arguments, source: &str, out: &mut String) {
    let mut parts: Vec<String> = arguments
        .args
        .iter()
        .map(|arg| canonical_expr(arg, source))
        .collect();
    for keyword in &arguments.keywords {
        let value = canonical_expr(&keyword.value, source);
        parts.push(match &keyword.arg {
            Some(name) => format!("{name}={value}"),
            None => format!("**{value}"),
        });
    }
    out.push('(');
    out.push_str(&parts.join(", "));
    out.push(')');
}

fn write_generators(generators: &[ast::Comprehension], source: &str, out: &mut String) {
    for generator in generators {
        out.push_str(if generator.is_async {
            " async for "
        } else {
            " for "
        });
        write_expr(&generator.target, source, out);
        out.push_str(" in ");
        write_expr(&generator.iter, source, out);
        for condition in &generator.ifs {
            out.push_str(" if ");
            write_expr(condition, source, out);
        }
    }
}

fn write_optional(expr: Option<&Expr>, source: &str, out: &mut String) {
    if let Some(expr) = expr {
        write_expr(expr, source, out);
    }
}

fn write_expr(expr: &Expr, source: &str, out: &mut String) {
    match expr {
        Expr::Name(e) => out.push_str(e.id.as_str()),
        Expr::StringLiteral(e) => out.push_str(&format!("{:?}", e.value.to_str())),
        Expr::BytesLiteral(e) => {
            let bytes: Vec<u8> = e.value.bytes().collect();
            out.push_str(&format!("b{:?}", String::from_utf8_lossy(&bytes)));
            // Bytes that are not UTF-8 would all read as U+FFFD: add them exactly.
            if std::str::from_utf8(&bytes).is_err() {
                out.push_str(&format!("{bytes:?}"));
            }
        }
        Expr::NumberLiteral(e) => match &e.value {
            Number::Int(i) => out.push_str(&i.to_string()),
            Number::Float(f) => out.push_str(&format!("{f:?}")),
            Number::Complex { real, imag } => out.push_str(&format!("({real:?}+{imag:?}j)")),
        },
        Expr::BooleanLiteral(e) => out.push_str(if e.value { "True" } else { "False" }),
        Expr::NoneLiteral(_) => out.push_str("None"),
        Expr::EllipsisLiteral(_) => out.push_str("..."),
        Expr::Attribute(e) => {
            write_expr(&e.value, source, out);
            out.push('.');
            out.push_str(e.attr.as_str());
        }
        Expr::Call(e) => {
            write_expr(&e.func, source, out);
            write_arguments(&e.arguments, source, out);
        }
        Expr::List(e) => write_list(&e.elts, "[", "]", source, out),
        Expr::Tuple(e) => write_list(&e.elts, "(", ",)", source, out),
        Expr::Set(e) => write_list(&e.elts, "{", "}", source, out),
        Expr::Dict(e) => {
            out.push('{');
            for (i, item) in e.items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match &item.key {
                    Some(key) => {
                        write_expr(key, source, out);
                        out.push_str(": ");
                    }
                    None => out.push_str("**"),
                }
                write_expr(&item.value, source, out);
            }
            out.push('}');
        }
        Expr::Subscript(e) => {
            write_expr(&e.value, source, out);
            out.push('[');
            write_expr(&e.slice, source, out);
            out.push(']');
        }
        Expr::Slice(e) => {
            write_optional(e.lower.as_deref(), source, out);
            out.push(':');
            write_optional(e.upper.as_deref(), source, out);
            out.push(':');
            write_optional(e.step.as_deref(), source, out);
        }
        Expr::Starred(e) => {
            out.push('*');
            write_expr(&e.value, source, out);
        }
        Expr::UnaryOp(e) => {
            out.push('(');
            out.push_str(e.op.as_str());
            out.push(' ');
            write_expr(&e.operand, source, out);
            out.push(')');
        }
        Expr::BinOp(e) => {
            out.push('(');
            write_expr(&e.left, source, out);
            out.push(' ');
            out.push_str(e.op.as_str());
            out.push(' ');
            write_expr(&e.right, source, out);
            out.push(')');
        }
        Expr::BoolOp(e) => {
            out.push('(');
            for (i, value) in e.values.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                    out.push_str(e.op.as_str());
                    out.push(' ');
                }
                write_expr(value, source, out);
            }
            out.push(')');
        }
        Expr::Compare(e) => {
            out.push('(');
            write_expr(&e.left, source, out);
            for (op, comparator) in e.ops.iter().zip(e.comparators.iter()) {
                out.push(' ');
                out.push_str(op.as_str());
                out.push(' ');
                write_expr(comparator, source, out);
            }
            out.push(')');
        }
        Expr::If(e) => {
            out.push('(');
            write_expr(&e.body, source, out);
            out.push_str(" if ");
            write_expr(&e.test, source, out);
            out.push_str(" else ");
            write_expr(&e.orelse, source, out);
            out.push(')');
        }
        Expr::Named(e) => {
            out.push('(');
            write_expr(&e.target, source, out);
            out.push_str(" := ");
            write_expr(&e.value, source, out);
            out.push(')');
        }
        Expr::ListComp(e) => {
            out.push('[');
            write_expr(&e.elt, source, out);
            write_generators(&e.generators, source, out);
            out.push(']');
        }
        Expr::SetComp(e) => {
            out.push('{');
            write_expr(&e.elt, source, out);
            write_generators(&e.generators, source, out);
            out.push('}');
        }
        Expr::Generator(e) => {
            out.push('(');
            write_expr(&e.elt, source, out);
            write_generators(&e.generators, source, out);
            out.push(')');
        }
        Expr::DictComp(e) => {
            out.push('{');
            match &e.key {
                Some(key) => {
                    write_expr(key, source, out);
                    out.push_str(": ");
                }
                None => out.push_str("**"),
            }
            write_expr(&e.value, source, out);
            write_generators(&e.generators, source, out);
            out.push('}');
        }
        Expr::Await(e) => {
            out.push_str("(await ");
            write_expr(&e.value, source, out);
            out.push(')');
        }
        Expr::Yield(e) => {
            out.push_str("(yield ");
            write_optional(e.value.as_deref(), source, out);
            out.push(')');
        }
        Expr::YieldFrom(e) => {
            out.push_str("(yield from ");
            write_expr(&e.value, source, out);
            out.push(')');
        }
        // Copied as written: their canonical form would need the whole grammar of format
        // specs and parameter lists.
        Expr::FString(_) | Expr::TString(_) | Expr::Lambda(_) | Expr::IpyEscapeCommand(_) => {
            let range = expr.range();
            out.push_str(&source[range.start().to_usize()..range.end().to_usize()]);
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_ast::Stmt;
    use ruff_python_parser::parse_module;

    /// The definition text of the first function in `source`.
    fn text(source: &str) -> String {
        definition_of(source, |d| d.text.clone())
    }

    fn definition_of<T>(source: &str, read: impl FnOnce(&Definition) -> T) -> T {
        let source = &format!("from barca import asset, sensor, task, sink, unsafe\n{source}");
        let parsed = parse_module(source).expect("test source parses");
        let func = parsed
            .syntax()
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::FunctionDef(f) => Some(f),
                _ => None,
            })
            .expect("a function");
        read(
            &node_definition(
                func,
                source,
                &crate::decorator_args::BarcaNames::of(&parsed.syntax().body),
            )
            .expect("a node"),
        )
    }

    /// The canonical form of each followed expression.
    fn followed(source: &str) -> Vec<String> {
        definition_of(source, |d| {
            d.followed
                .iter()
                .map(|e| canonical_expr(e, source))
                .collect()
        })
    }

    fn expr(source: &str) -> String {
        let parsed = ruff_python_parser::parse_expression(source).expect("expression parses");
        canonical_expr(&parsed.syntax().body, source)
    }

    const BODY: &str = "def sales(region: str) -> dict:\n    return {\"region\": region}\n";

    #[test]
    fn a_function_that_is_not_a_node_has_no_definition() {
        let source = "@functools.cache\ndef helper():\n    return 1\n";
        let parsed = parse_module(source).unwrap();
        let Stmt::FunctionDef(func) = &parsed.syntax().body[0] else {
            panic!("a function");
        };
        assert!(
            node_definition(
                func,
                source,
                &crate::decorator_args::BarcaNames::of(&parsed.syntax().body)
            )
            .is_none()
        );
    }

    /// The exact text, pinned: a change here changes every definition hash.
    #[test]
    fn the_text_is_the_counted_parts_then_the_function_from_def() {
        let source = "\
@functools.lru_cache(maxsize=None)
@asset(
    serializer='json',  # a comment
    inputs={\"b\": up_b, 'a': collect(up_a)},
    partitions={\"region\": partitions([\"us\", \"eu\"])},
    description=\"sales by region\",
    retries=3,
)
@sink(\"out.json\", serializer=\"json\")
def sales(region: str, a: list, b: int) -> dict:
    # the function is hashed as written, comments included
    return {'region': region}
";
        assert_eq!(
            text(source),
            "\
decorator functools.lru_cache(maxsize=None)
decorator sink(\"out.json\", serializer=\"json\")
argument inputs={\"a\": collect(up_a), \"b\": up_b}
argument partitions={\"region\": keys}
argument serializer=\"json\"
def sales(region: str, a: list, b: int) -> dict:
    # the function is hashed as written, comments included
    return {'region': region}"
        );
    }

    #[test]
    fn a_bare_decorator_and_an_empty_call_are_the_same() {
        let bare = text(&format!("@asset\n{BODY}"));
        assert_eq!(bare, BODY.trim_end());
        assert_eq!(bare, text(&format!("@asset()\n{BODY}")));
        assert_eq!(bare, text(&format!("@asset(  )\n\n# note\n{BODY}")));
    }

    #[test]
    fn formatting_of_the_decorator_does_not_change_the_text() {
        let one = text(&format!(
            "@asset(inputs={{\"x\": up}}, serializer=\"json\", partitions={{\"region\": partitions_from(up)}})\n{BODY}"
        ));
        let other = text(&format!(
            "@asset(\n    # which region\n    partitions = {{ 'region' : partitions_from( up ) , }},\n    serializer = ('json'),\n    inputs = {{\n        'x': up,  # upstream\n    }},\n)\n{BODY}"
        ));
        assert_eq!(one, other);
    }

    #[test]
    fn arguments_that_do_not_count_do_not_change_the_text() {
        let plain = text(&format!("@asset()\n{BODY}"));
        for arguments in [
            "description=\"d\"",
            "tags={\"team\": \"a\"}",
            "retries=5",
            "retry_backoff=2.5",
            "timeout_seconds=10",
            "freshness=Manual",
            "freshness=Schedule(\"0 * * * *\")",
            "name=\"other\"",
            "env=[\"A\", \"B\"]",
            "description=DESCRIPTION, tags=TAGS, retries=RETRIES",
        ] {
            assert_eq!(
                plain,
                text(&format!("@asset({arguments})\n{BODY}")),
                "{arguments}"
            );
        }
        assert_eq!(plain, text(&format!("@asset()\n@unsafe\n{BODY}")));
    }

    #[test]
    fn each_counted_part_changes_the_text() {
        let base = text(&format!(
            "@asset(inputs={{\"x\": up}}, serializer=\"json\", partitions={{\"region\": partitions([\"us\"])}})\n@sink(\"a.json\")\n{BODY}"
        ));
        for (what, changed) in [
            (
                "inputs: another upstream",
                "@asset(inputs={\"x\": other}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\")\n",
            ),
            (
                "inputs: another parameter",
                "@asset(inputs={\"y\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\")\n",
            ),
            (
                "inputs: collect",
                "@asset(inputs={\"x\": collect(up)}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\")\n",
            ),
            (
                "serializer",
                "@asset(inputs={\"x\": up}, serializer=\"pickle\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\")\n",
            ),
            (
                "partitions: the dimension name",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"area\": partitions([\"us\"])})\n@sink(\"a.json\")\n",
            ),
            (
                "partitions: a second dimension",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"]), \"tier\": partitions([1])})\n@sink(\"a.json\")\n",
            ),
            (
                "partitions: partitions_from instead of partitions",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions_from(up)})\n@sink(\"a.json\")\n",
            ),
            (
                "sink: the path",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"b.json\")\n",
            ),
            (
                "sink: its serializer",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\", serializer=\"pickle\")\n",
            ),
            (
                "sink: removed",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n",
            ),
            (
                "another decorator",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])})\n@sink(\"a.json\")\n@functools.cache\n",
            ),
            (
                "an argument barca does not know",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])}, mode=\"fast\")\n@sink(\"a.json\")\n",
            ),
            (
                "a **mapping",
                "@asset(inputs={\"x\": up}, serializer=\"json\", partitions={\"region\": partitions([\"us\"])}, **OPTIONS)\n@sink(\"a.json\")\n",
            ),
        ] {
            assert_ne!(base, text(&format!("{changed}{BODY}")), "{what}");
        }
    }

    #[test]
    fn the_partitions_from_source_counts() {
        let from = |source: &str| {
            text(&format!(
                "@asset(partitions={{\"region\": partitions_from({source})}})\n{BODY}"
            ))
        };
        assert_ne!(from("sales"), from("orders"));
        assert_eq!(from("sales"), from(" sales "));
    }

    /// Issue #283: the keys of a dimension are not part of the definition, in any form.
    #[test]
    fn the_partition_keys_do_not_change_the_text() {
        let keys = |keys: &str| {
            text(&format!(
                "@asset(partitions={{\"region\": partitions({keys})}})\n{BODY}"
            ))
        };
        let base = keys("[\"us\", \"eu\"]");
        for other in [
            "[\"us\", \"eu\", \"apac\"]",
            "[\"us\"]",
            "[\"eu\", \"us\"]",
            "[]",
            "REGIONS",
            "REGIONS + [\"apac\"]",
            "regions()",
            "[r for r in (\"us\", \"eu\", \"apac\")]",
            "[1, 2, 3]",
        ] {
            assert_eq!(base, keys(other), "{other}");
        }
    }

    #[test]
    fn other_decorators_keep_their_order_and_barca_s_position_among_them_does_not_matter() {
        let a = text(&format!("@asset()\n@first\n@second(1)\n{BODY}"));
        let b = text(&format!("@first\n@asset()\n@second(1)\n{BODY}"));
        let c = text(&format!("@first\n@second(1)\n@asset()\n{BODY}"));
        assert_eq!(a, b);
        assert_eq!(a, c);
        let swapped = text(&format!("@asset()\n@second(1)\n@first\n{BODY}"));
        assert_ne!(a, swapped);
    }

    #[test]
    fn sensors_and_tasks_follow_the_same_rules() {
        for kind in ["sensor", "task"] {
            let plain = text(&format!("@{kind}()\n{BODY}"));
            assert_eq!(plain, BODY.trim_end());
            assert_eq!(
                plain,
                text(&format!(
                    "@{kind}(freshness=Schedule(\"*/5 * * * *\"), description=\"d\", retries=2)\n{BODY}"
                ))
            );
            assert_ne!(plain, text(&format!("@{kind}()\n@traced\n{BODY}")));
        }
        assert_ne!(
            text(&format!("@task(inputs={{\"x\": a}})\n{BODY}")),
            text(&format!("@task(inputs={{\"x\": b}})\n{BODY}"))
        );
    }

    #[test]
    fn the_function_starts_at_def_or_async() {
        assert_eq!(
            text("@asset()\n# a comment between\n\ndef f():\n    return 1\n"),
            "def f():\n    return 1"
        );
        assert_eq!(
            text("@asset()\nasync def f():\n    return 1\n"),
            "async def f():\n    return 1"
        );
        assert_eq!(
            text("@asset()\nasync  def  f():\n    return 1\n"),
            "async  def  f():\n    return 1"
        );
        // A name that ends in `def` or `async` is not the keyword.
        assert_eq!(
            text("@asset()\ndef undef():\n    return 1\n"),
            "def undef():\n    return 1"
        );
    }

    #[test]
    fn names_in_counted_parts_are_followed_and_node_references_are_not() {
        let source = "\
@retry(times=ATTEMPTS)
@asset(
    inputs={\"a\": up, \"b\": collect(mod.up), \"c\": asset_ref(\"p.py:up\"), KEY: up},
    serializer=FMT,
    partitions={\"region\": partitions(REGIONS), DIM: partitions_from(keys)},
    description=DESCRIPTION,
    tags=TAGS,
    retries=RETRIES,
    freshness=Schedule(CRON),
    codec=make_codec(LEVEL),
)
@sink(\"out.json\", serializer=SINK_FMT)
def f(a, b, c):
    return 1
";
        let mut got = followed(source);
        got.sort();
        assert_eq!(
            got,
            [
                "\"out.json\"",
                "\"p.py:up\"",
                "DIM",
                "FMT",
                "KEY",
                "SINK_FMT",
                "make_codec(LEVEL)",
                "retry(times=ATTEMPTS)",
            ]
        );
    }

    #[test]
    fn canonical_expressions() {
        for (a, b) in [
            ("'a'", "\"a\""),
            ("'a' 'b'", "\"ab\""),
            ("r'a'", "'a'"),
            ("[1,2,]", "[ 1 , 2 ]"),
            ("(x)", "x"),
            ("0x10", "16"),
            ("{'a':1}", "{ \"a\" : 1, }"),
            ("a+b*c", "a + (b * c)"),
            ("not a", "(not a)"),
            ("a.b . c", "a.b.c"),
            ("x[1:2]", "x[ 1 : 2 ]"),
            (
                "[i for i in range(3) if i]",
                "[ i  for  i  in  range( 3 )  if  i ]",
            ),
        ] {
            assert_eq!(expr(a), expr(b), "{a} / {b}");
        }
        for (a, b) in [
            ("'a'", "'b'"),
            ("'a'", "b'a'"),
            ("1", "1.0"),
            ("1", "'1'"),
            ("[1, 2]", "[2, 1]"),
            ("(1, 2)", "[1, 2]"),
            ("(1,)", "1"),
            ("f(1, 2)", "f(2, 1)"),
            ("f(b=1, a=2)", "f(a=2, b=1)"),
            ("f(a=1, **extras)", "f(**extras, a=1)"),
            ("f(a=1)", "f(1)"),
            ("a + b", "a - b"),
            ("(a + b) * c", "a + b * c"),
            ("a < b", "a <= b"),
            ("a and b", "a or b"),
            ("x[1]", "x(1)"),
            ("[*a]", "[a]"),
            ("{1}", "[1]"),
            ("True", "False"),
            ("None", "..."),
            ("a if b else c", "c if b else a"),
            ("{'a': 1, 'b': 2}", "{'b': 2, 'a': 1}"),
        ] {
            assert_ne!(expr(a), expr(b), "{a} / {b}");
        }
        // Written out, once, so the form itself is pinned.
        assert_eq!(
            expr("f(x, *rest, k = [1, 'two', 3.0, None, True], **{'a': -b.c[0]})"),
            "f(x, *rest, k=[1, \"two\", 3.0, None, True], **{\"a\": (- b.c[0])})"
        );
    }

    #[test]
    fn node_hash_rules_match_exactly_the_accepted_signature_pairs() {
        use std::collections::BTreeSet;
        let accepted: BTreeSet<_> = crate::decorator_args::arguments()
            .filter(|argument| matches!(argument.call, "asset" | "sensor" | "task"))
            .map(|argument| (argument.call, argument.name))
            .collect();
        let mapped: Vec<_> = node_argument_rules().collect();
        let pairs: BTreeSet<_> = mapped
            .iter()
            .map(|(argument, _)| (argument.call, argument.name))
            .collect();
        assert_eq!(pairs, accepted);
        assert_eq!(mapped.len(), pairs.len(), "duplicate argument rules");
        for (argument, rule) in mapped {
            assert_eq!(argument.name, rule.part);
            assert_eq!(argument.passing, crate::decorator_args::Passing::Keyword);
        }
        // A rule for a removed keyword must not remain silently in the hash/manual table.
        let accepted_names: BTreeSet<_> = accepted.iter().map(|(_, name)| *name).collect();
        let keyword_rules: Vec<_> = RULES
            .iter()
            .filter(|rule| ![SINK, OTHER_DECORATOR, UNKNOWN_ARGUMENT, UNSAFE].contains(&rule.part))
            .map(|rule| rule.part)
            .collect();
        let names: BTreeSet<_> = keyword_rules.iter().copied().collect();
        assert_eq!(names, accepted_names);
        assert_eq!(keyword_rules.len(), names.len(), "duplicate keyword rules");
    }

    /// `barca docs cache` lists exactly [`RULES`]: the same parts, in the same order, with the
    /// same answer and the same reason.
    #[test]
    fn the_manual_lists_exactly_these_rules() {
        let manual = include_str!("../../barca-cli/docs/cache.md");
        let section = manual
            .split("#### Which decorator arguments count")
            .nth(1)
            .expect("cache.md has the section 'Which decorator arguments count'");
        let section = section.split("\n#").next().unwrap();
        let rows: Vec<Vec<String>> = section
            .lines()
            .filter(|line| line.starts_with('|'))
            .skip(2) // header and separator
            .map(|line| {
                line.trim_matches('|')
                    .split('|')
                    .map(|cell| cell.trim().to_string())
                    .collect()
            })
            .collect();
        let expected: Vec<Vec<String>> = RULES
            .iter()
            .map(|rule| {
                let part = if rule.part.starts_with('@') || rule.part.starts_with("any ") {
                    rule.part.to_string()
                } else {
                    format!("{}=", rule.part)
                };
                let part = if rule.part.starts_with("any ") {
                    part
                } else {
                    format!("`{part}`")
                };
                let counts = match rule.counts {
                    Counts::Yes => "yes",
                    Counts::No => "no",
                    Counts::PartitionShape => "the shape, not the keys",
                };
                vec![part, counts.to_string(), rule.reason.to_string()]
            })
            .collect();
        assert_eq!(rows, expected);
    }
}
