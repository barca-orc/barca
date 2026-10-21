//! `inputs=` keys and partition dimensions against the function's parameters (#549).
//!
//! The worker calls a step as `fn(**kwargs)`: one keyword per `inputs=` key and per partition
//! dimension (plus, for `partitions_from(upstream)` on a partitioned upstream, that upstream's
//! output under its name). A key that is not a parameter, or a parameter that nothing fills and
//! that has no default, is a `TypeError` in the worker, after every upstream has run. Both are
//! visible in the source, so they are reported when the file is read, exit 2.
//!
//! Only what is certain is reported:
//!
//! - A function with `**kwargs` accepts any key.
//! - When `inputs=` or `partitions=` is not a literal dict with literal string keys (a variable,
//!   a `**spread`), which keywords arrive cannot be known, so no parameter is reported missing.
//! - A parameter named after a `partitions_from(...)` source counts as filled: barca passes the
//!   source's output under that name when the source is partitioned.
//! - A `@task`'s parameters are not reported missing: a task with parameters is how a
//!   `parallel()` branch receives its arguments (`partial(check, region)`).

use ruff_python_ast::{self as ast, Expr, Keyword};
use ruff_text_size::Ranged;

use crate::decorator_args::Problem;
use crate::model::NodeKind;

/// The first mismatch between what barca passes `func` and what `func` takes, if any.
pub(crate) fn check(
    func: &ast::StmtFunctionDef,
    kind: NodeKind,
    keywords: &[&Keyword],
    barca: &crate::decorator_args::BarcaNames,
) -> Option<Problem> {
    let params = &func.parameters;
    let name = func.name.as_str();
    // Every parameter a keyword can fill, in declaration order.
    let named: Vec<&ast::ParameterWithDefault> =
        params.args.iter().chain(&params.kwonlyargs).collect();
    let names: Vec<&str> = named.iter().map(|p| p.name().as_str()).collect();
    let takes = if names.is_empty() {
        format!("{name}() takes no parameters")
    } else {
        format!(
            "{name}() takes {}",
            names
                .iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let see = "See `barca docs assets`.";

    // What barca passes: (keyword, its offset, which argument it comes from).
    let mut passed: Vec<(String, usize, &'static str)> = Vec::new();
    // Whether `passed` is everything barca passes.
    let mut complete = true;
    for kw in keywords {
        let Some(arg) = kw.arg.as_ref().map(|a| a.as_str()) else {
            continue;
        };
        let what = match arg {
            "inputs" => "inputs=",
            "partitions" => "partitions=",
            _ => continue,
        };
        let Expr::Dict(dict) = &kw.value else {
            complete = false;
            continue;
        };
        for item in &dict.items {
            // Only entries the parser reads are passed (`crate::parse`): an upstream for
            // `inputs=`, a barca `partitions(...)` / `partitions_from(...)` for `partitions=`.
            let helper = match &item.value {
                Expr::Call(call) => barca.recognized(&call.func),
                _ => None,
            };
            let read = match what {
                "inputs=" => matches!(
                    item.value,
                    Expr::Name(_) | Expr::Attribute(_) | Expr::Call(_)
                ),
                _ => matches!(helper, Some("partitions" | "partitions_from")),
            };
            match item.key.as_ref() {
                Some(Expr::StringLiteral(key)) if read => {
                    passed.push((key.value.to_string(), key.range().start().to_usize(), what));
                }
                _ => complete = false,
            }
            // `partitions={"region": partitions_from(sales)}` also passes `sales`.
            if what == "partitions="
                && helper == Some("partitions_from")
                && let Expr::Call(call) = &item.value
                && let Some(source) = call.arguments.args.first().and_then(last_name)
            {
                passed.push((source, item.value.range().start().to_usize(), ""));
            }
        }
    }

    if params.kwarg.is_none() {
        for (key, offset, what) in &passed {
            if what.is_empty() || names.contains(&key.as_str()) {
                continue;
            }
            let free: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| !passed.iter().any(|(k, _, _)| k == n))
                .collect();
            let guess = crate::suggest::closest(key, free.iter().copied());
            let meant = crate::suggest::did_you_mean(&guess, '`')
                .map(|m| format!(" {m}"))
                .unwrap_or_default();
            let positional_only = params.posonlyargs.iter().any(|p| p.name().as_str() == key);
            let (message, fix) = if positional_only {
                (
                    format!(
                        "the {what} key `{key}` names a positional-only parameter of {name}(); \
                         barca passes inputs by keyword."
                    ),
                    format!("Remove the `/` after `{key}` in the definition of {name}(). {see}"),
                )
            } else {
                let fix = match guess.as_slice() {
                    [g] => {
                        format!("Rename the key to `{g}`, or add a parameter `{key}` to {name}()")
                    }
                    [a, b, ..] => format!(
                        "Rename the key to `{a}` or `{b}`, or add a parameter `{key}` to {name}()"
                    ),
                    [] if names.is_empty() => {
                        format!("Add a parameter `{key}` to {name}(), or remove the key")
                    }
                    [] => format!(
                        "Rename the key to one of the parameters, or add a parameter `{key}` to \
                         {name}()"
                    ),
                };
                (
                    format!("the {what} key `{key}` is not a parameter: {takes}.{meant}"),
                    format!(
                        "{fix}. Barca passes each {what} key to the function as a keyword \
                         argument. {see}"
                    ),
                )
            };
            return Some(Problem {
                offset: *offset,
                message,
                fix,
            });
        }
    }

    if kind == NodeKind::Task || !complete {
        return None;
    }
    let required = params
        .posonlyargs
        .iter()
        .chain(named.iter().copied())
        .filter(|p| p.default.is_none());
    for param in required {
        let missing = param.name().as_str();
        if passed.iter().any(|(k, _, _)| k == missing) {
            continue;
        }
        let unused: Vec<&str> = passed
            .iter()
            .filter(|(k, _, what)| !what.is_empty() && !names.contains(&k.as_str()))
            .map(|(k, _, _)| k.as_str())
            .collect();
        let guess = crate::suggest::closest(missing, unused.iter().copied());
        let meant = match guess.as_slice() {
            [g] => format!(" (the key `{g}` looks like it was meant for it)"),
            _ => String::new(),
        };
        let given = {
            let keys: Vec<String> = passed
                .iter()
                .filter(|(_, _, what)| !what.is_empty())
                .map(|(k, _, _)| format!("`{k}`"))
                .collect();
            if keys.is_empty() {
                "barca passes it no arguments".to_string()
            } else {
                format!("barca passes it {}", keys.join(", "))
            }
        };
        return Some(Problem {
            offset: param.range().start().to_usize(),
            message: format!(
                "the parameter `{missing}` of {name}() has no input and no default{meant}: \
                 {given}, so calling it would fail."
            ),
            fix: format!(
                "Wire it with `inputs={{\"{missing}\": <upstream>}}`, give it a default \
                 (`{missing}=...`), or remove it. {see}"
            ),
        });
    }
    None
}

/// The name a `partitions_from(...)` argument is passed under: `sales`, `m.sales`.
fn last_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(a.attr.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::parse::extract_nodes;

    fn error(src: &str) -> String {
        extract_nodes(src, "p.py").unwrap_err().to_string()
    }

    fn ok(src: &str) {
        extract_nodes(src, "p.py").unwrap();
    }

    #[test]
    fn an_input_key_that_is_not_a_parameter_is_an_error_with_a_guess() {
        let err = error(
            "from barca import asset\n\n@asset()\ndef raw() -> list:\n    return [1]\n\n\
             @asset(inputs={\"row\": raw})\ndef amounts(rows: list) -> list:\n    return rows\n",
        );
        assert!(err.starts_with("p.py:amounts (line 7): "), "{err}");
        assert!(
            err.contains("the inputs= key `row` is not a parameter: amounts() takes `rows`."),
            "{err}"
        );
        assert!(err.contains("Did you mean `rows`?"), "{err}");
        assert!(err.contains("\nRename the key to `rows`"), "{err}");
    }

    #[test]
    fn a_parameter_with_no_input_and_no_default_is_an_error() {
        let err = error(
            "from barca import asset\n\n@asset()\ndef amounts(\n    rows: list,\n) -> list:\n    \
             return rows\n",
        );
        assert!(err.starts_with("p.py:amounts (line 5): "), "{err}");
        assert!(
            err.contains(
                "the parameter `rows` of amounts() has no input and no default: barca passes \
                 it no arguments"
            ),
            "{err}"
        );
        assert!(err.contains("inputs={\"rows\": <upstream>}"), "{err}");
    }

    #[test]
    fn defaults_and_kwargs_are_accepted() {
        ok(
            "from barca import asset\n\n@asset()\ndef a(limit: int = 3, *, b=1) -> int:\n    \
            return limit\n",
        );
        ok(
            "from barca import asset\n\n@asset()\ndef up() -> int:\n    return 1\n\n\
            @asset(inputs={\"anything\": up})\ndef a(**kw) -> int:\n    return 1\n",
        );
        // `**kwargs` takes any key but does not fill a required parameter.
        let err = error(
            "from barca import asset\n\n@asset()\ndef up() -> int:\n    return 1\n\n\
             @asset(inputs={\"anything\": up})\ndef a(x, **kw) -> int:\n    return 1\n",
        );
        assert!(err.contains("the parameter `x` of a()"), "{err}");
    }

    #[test]
    fn keyword_only_parameters_can_be_inputs() {
        ok(
            "from barca import asset\n\n@asset()\ndef up() -> int:\n    return 1\n\n\
            @asset(inputs={\"x\": up})\ndef a(*, x) -> int:\n    return x\n",
        );
    }

    #[test]
    fn a_positional_only_input_is_an_error() {
        let err = error(
            "from barca import asset\n\n@asset()\ndef up() -> int:\n    return 1\n\n\
             @asset(inputs={\"x\": up})\ndef a(x, /) -> int:\n    return x\n",
        );
        assert!(err.contains("positional-only"), "{err}");
    }

    #[test]
    fn partition_dimensions_fill_parameters_and_must_be_parameters() {
        ok("from barca import asset, partitions\n\n\
            @asset(partitions={\"region\": partitions([\"a\", \"b\"])})\n\
            def sales(region: str) -> str:\n    return region\n");
        let err = error(
            "from barca import asset, partitions\n\n\
             @asset(partitions={\"regoin\": partitions([\"a\"])})\n\
             def sales(region: str) -> str:\n    return region\n",
        );
        assert!(
            err.contains("the partitions= key `regoin` is not a parameter"),
            "{err}"
        );
        assert!(err.contains("Did you mean `region`?"), "{err}");
    }

    #[test]
    fn a_partitions_from_source_fills_the_parameter_named_after_it() {
        ok("from barca import asset, partitions, partitions_from\n\n\
            @asset(partitions={\"region\": partitions([\"a\"])})\n\
            def sales(region: str) -> str:\n    return region\n\n\
            @asset(partitions={\"region\": partitions_from(sales)})\n\
            def margin(region: str, sales: str) -> str:\n    return sales\n");
        // An unpartitioned source's list gives only the keys.
        ok(
            "from barca import asset, partitions_from\n\n@asset()\ndef keys() -> list:\n    \
            return [\"a\"]\n\n@asset(partitions={\"k\": partitions_from(keys)})\n\
            def per_key(k: str) -> str:\n    return k\n",
        );
    }

    #[test]
    fn inputs_that_cannot_be_read_statically_report_no_missing_parameter() {
        ok(
            "from barca import asset\n\nINPUTS = {}\n\n@asset(inputs=INPUTS)\n\
            def a(x) -> int:\n    return x\n",
        );
    }

    #[test]
    fn a_task_may_take_parameters_for_parallel_branches() {
        ok(
            "from barca import task\n\n@task()\ndef check(region: str) -> dict:\n    \
            return {}\n",
        );
        let err = error(
            "from barca import task, asset\n\n@asset()\ndef up() -> int:\n    return 1\n\n\
             @task(inputs={\"upp\": up})\ndef t(up: int) -> None:\n    pass\n",
        );
        assert!(err.contains("Did you mean `up`?"), "{err}");
    }

    #[test]
    fn a_sensor_parameter_needs_a_value() {
        let err =
            error("from barca import sensor\n\n@sensor()\ndef s(path) -> str:\n    return path\n");
        assert!(err.contains("the parameter `path` of s()"), "{err}");
    }
}
