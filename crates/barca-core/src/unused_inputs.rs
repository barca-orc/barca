//! Static check: which declared data inputs does a step's function never use? (#231)
//!
//! Pure function over the function's AST, called by the parser while it already holds the
//! parsed file, so the check costs one extra walk of each decorated function body and no
//! second parse. Nothing is imported or executed.
//!
//! The rule is conservative: a warning must mean the input is really unused, so every case the
//! analysis cannot see through reports nothing. See `barca docs assets`, "Unused inputs".

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr, Stmt};

use std::collections::HashMap;

use crate::model::{DeclaredInput, ValueType};

/// Names through which a function can reach its own parameters without naming them. A body
/// that mentions any of these, as a bare name (also when imported under another name, see
/// `imported_name` below) or as an attribute (`builtins.locals`, `frame.f_locals`,
/// `sys._getframe`), is never reported. This is the one place the list lives; the manual
/// (`assets.md`, "Unused inputs") repeats it and a test keeps the two in step.
pub const DYNAMIC_ACCESS: &[&str] = &[
    "locals",
    "vars",
    "eval",
    "exec",
    "currentframe",
    "_getframe",
    "f_locals",
    "getargvalues",
];

/// The data inputs of `func` that its body never uses, in declaration order.
///
/// A data input is a parameter wired by `inputs=` whose name does not start with `_`
/// (`_`-prefixed inputs are ordering-only: declared unused on purpose, never reported).
/// It is unused when the body never mentions its name, or mentions it only as the target of
/// `del name`. Any other mention counts as a use, in any position and any nested scope:
/// reading it, passing it to a helper, a closure, a comprehension, an f-string, assigning to it.
///
/// An input annotated `duckdb.DuckDBPyRelation` is never reported: barca also binds it as a
/// view named after the parameter, so SQL text anywhere (a helper module included) can read it
/// without the Python name appearing in the body.
///
/// Nothing is reported for the whole function when
/// - it takes `**kwargs` (inputs are passed by keyword, so they can arrive there),
/// - its body has no real statement: only a docstring, `pass`, `...` or `raise` (a stub, or a
///   gate that only raises, uses nothing by definition),
/// - its body mentions one of [`DYNAMIC_ACCESS`].
///
/// `imported_name` maps a name bound by `from m import x as y` at the top of the file to the
/// name it was imported as (`y` -> `x`), so `from inspect import currentframe as cf` is seen.
pub fn unused_inputs<'n>(
    func: &ast::StmtFunctionDef,
    inputs: &[DeclaredInput],
    param_types: &HashMap<String, ValueType>,
    imported_name: &'n dyn Fn(&str) -> Option<&'n str>,
) -> Vec<String> {
    let params = &func.parameters;
    let mut candidates: Vec<&str> = Vec::new();
    for input in inputs {
        let name = input.param_name.as_str();
        if !name.starts_with('_')
            && !candidates.contains(&name)
            && param_types.get(name) != Some(&ValueType::DuckDB)
            && params
                .iter_non_variadic_params()
                .any(|p| p.parameter.name.as_str() == name)
        {
            candidates.push(name);
        }
    }
    if candidates.is_empty() || params.kwarg.is_some() || is_stub(&func.body) {
        return Vec::new();
    }

    let mut uses = Uses {
        candidates: &candidates,
        used: vec![false; candidates.len()],
        dynamic: false,
        imported_name,
    };
    visitor::walk_body(&mut uses, &func.body);
    if uses.dynamic {
        return Vec::new();
    }
    candidates
        .iter()
        .zip(&uses.used)
        .filter(|(_, used)| !**used)
        .map(|(name, _)| name.to_string())
        .collect()
}

/// A body with no real statement: every statement is `pass`, `raise`, or a bare constant
/// (a docstring, `...`).
fn is_stub(body: &[Stmt]) -> bool {
    body.iter().all(|stmt| match stmt {
        Stmt::Pass(_) | Stmt::Raise(_) => true,
        Stmt::Expr(e) => matches!(
            &*e.value,
            Expr::StringLiteral(_)
                | Expr::EllipsisLiteral(_)
                | Expr::NoneLiteral(_)
                | Expr::NumberLiteral(_)
                | Expr::BooleanLiteral(_)
                | Expr::BytesLiteral(_)
        ),
        _ => false,
    })
}

struct Uses<'c, 'n> {
    candidates: &'c [&'c str],
    used: Vec<bool>,
    dynamic: bool,
    imported_name: &'n dyn Fn(&str) -> Option<&'n str>,
}

impl Uses<'_, '_> {
    fn mention(&mut self, name: &str) {
        if DYNAMIC_ACCESS.contains(&name) {
            self.dynamic = true;
        }
        if let Some(i) = self.candidates.iter().position(|c| *c == name) {
            self.used[i] = true;
        }
    }
}

impl<'a> Visitor<'a> for Uses<'_, '_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        if let Stmt::Delete(del) = stmt {
            // `del df` is not a use; `del df[0]` and `del df.x` read `df`.
            for target in &del.targets {
                if !matches!(target, Expr::Name(_)) {
                    self.visit_expr(target);
                }
            }
            return;
        }
        visitor::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                let name = n.id.as_str();
                self.mention(name);
                if let Some(original) = (self.imported_name)(name)
                    && DYNAMIC_ACCESS.contains(&original)
                {
                    self.dynamic = true;
                }
            }
            Expr::Attribute(a) if DYNAMIC_ACCESS.contains(&a.attr.as_str()) => {
                self.dynamic = true;
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use crate::parse::extract_nodes;

    /// The unused inputs of the last decorated function in `src`.
    fn unused_in(src: &str) -> Vec<String> {
        let nodes = extract_nodes(src, "t.py").unwrap();
        nodes.last().unwrap().unused_inputs.clone()
    }

    const HEAD: &str = "import builtins, inspect, sys\nimport duckdb\nimport polars as pl\n\
                        from barca import asset, task\n\
                        from inspect import currentframe as cf\n\
                        @asset\ndef up(): return 1\n";

    fn step(params: &str, body: &str, inputs: &str) -> String {
        format!("{HEAD}@asset(inputs={{{inputs}}})\ndef s({params}):\n{body}\n")
    }

    fn unused(params: &str, body: &str) -> Vec<String> {
        unused_in(&step(params, body, "\"a\": up"))
    }

    #[test]
    fn never_mentioned_is_reported() {
        assert_eq!(unused("a", "    return 1"), ["a"]);
        assert_eq!(unused("a: dict", "    x = compute()\n    return x"), ["a"]);
    }

    #[test]
    fn del_only_is_reported() {
        assert_eq!(unused("a", "    del a\n    return 1"), ["a"]);
        assert_eq!(unused("a", "    del (a)\n    return 1"), ["a"]);
    }

    #[test]
    fn any_other_mention_is_a_use() {
        for body in [
            "    return a",
            "    return helper(a)",
            "    return helper(x=a)",
            "    del a[0]\n    return 1",
            "    del a.col\n    return 1",
            "    return f\"{a}\"",
            "    return [x for x in a]",
            "    def inner():\n        return a\n    return inner()",
            "    f = lambda: a\n    return f()",
            "    class C:\n        v = a\n    return C",
            "    _ = a\n    return 1",
            "    a = other()\n    return 1",
            "    if a is None:\n        raise ValueError\n    return 1",
            "    with a:\n        return 1",
        ] {
            assert!(unused("a", body).is_empty(), "{body}");
        }
    }

    /// One case per entry of DYNAMIC_ACCESS (and the spellings the review found): none warns.
    #[test]
    fn dynamic_access_is_never_reported() {
        for body in [
            "    return locals()",
            "    return helper(**locals())",
            "    return builtins.locals()",
            "    return __builtins__.locals()['a']",
            "    f = locals\n    return f()",
            "    return vars()",
            "    return builtins.vars()['a']",
            "    return eval('a')",
            "    exec('print(a)')\n    return 1",
            "    return inspect.currentframe().f_locals",
            "    return helper(inspect.currentframe())",
            "    return cf().f_back",
            "    return sys._getframe().f_locals['a']",
            "    return sys._getframe(0)",
            "    return frame_of().f_locals",
            "    return inspect.getargvalues(frame_of())",
            "    def inner():\n        return locals()\n    return inner()",
        ] {
            assert!(unused("a", body).is_empty(), "{body}");
        }
    }

    #[test]
    fn every_dynamic_access_name_has_a_test_case_above() {
        // Each name, as a bare name and as an attribute, silences the check on its own.
        for name in super::DYNAMIC_ACCESS {
            assert!(
                unused("a", &format!("    return {name}")).is_empty(),
                "{name}"
            );
            assert!(
                unused("a", &format!("    return m.{name}")).is_empty(),
                "m.{name}"
            );
        }
        // A name that only looks similar does not.
        assert_eq!(unused("a", "    return local_values()"), ["a"]);
    }

    #[test]
    fn kwargs_is_never_reported() {
        assert!(unused("a, **kw", "    return helper(**kw)").is_empty());
        assert!(unused("**kw", "    return 1").is_empty());
    }

    #[test]
    fn stub_and_gate_bodies_are_never_reported() {
        for body in [
            "    pass",
            "    ...",
            "    \"\"\"Docstring only.\"\"\"",
            "    \"\"\"Docstring.\"\"\"\n    pass",
            "    raise NotImplementedError",
            "    raise ValueError(\"check failed on purpose\")",
            "    \"\"\"Gate.\"\"\"\n    raise RuntimeError(\"stop\")",
        ] {
            assert!(unused("a", body).is_empty(), "{body}");
        }
    }

    #[test]
    fn a_real_body_that_ignores_the_input_is_reported() {
        for body in [
            "    return None",
            "    return {}",
            "    print(\"hi\")",
            "    \"\"\"Docstring.\"\"\"\n    return 1",
            "    if flag():\n        raise ValueError\n    return 1",
        ] {
            assert_eq!(unused("a", body), ["a"], "{body}");
        }
    }

    #[test]
    fn underscore_inputs_are_never_reported() {
        assert!(unused_in(&step("_a", "    return 1", "\"_a\": up")).is_empty());
        assert!(unused_in(&step("_a", "    del _a\n    return 1", "\"_a\": up")).is_empty());
    }

    #[test]
    fn duckdb_relation_inputs_are_never_reported() {
        // Bound as a view named `a`: SQL in the body, or in a helper, reads it by name.
        for body in [
            "    return duckdb.sql(\"select * from a\")",
            "    return query_in_a_helper_module()",
        ] {
            assert!(
                unused("a: duckdb.DuckDBPyRelation", body).is_empty(),
                "{body}"
            );
        }
        // Other annotations, lazy or not, are judged by the Python name.
        assert_eq!(unused("a: pl.LazyFrame", "    return 1"), ["a"]);
        assert_eq!(unused("a: pl.DataFrame", "    return 1"), ["a"]);
    }

    #[test]
    fn only_the_unused_ones_are_named_in_declaration_order() {
        let src = step("a, b, c", "    return b", "\"a\": up, \"b\": up, \"c\": up");
        assert_eq!(unused_in(&src), ["a", "c"]);
    }

    #[test]
    fn an_input_missing_from_the_signature_is_not_reported() {
        // The call fails at run time (unexpected keyword); that is not this warning's business.
        assert!(unused_in(&step("", "    return 1", "\"a\": up")).is_empty());
    }

    #[test]
    fn collected_inputs_tasks_and_partition_parameters() {
        let collected = format!(
            "{HEAD}from barca import collect\n@asset(inputs={{\"parts\": collect(up)}})\n\
             def s(parts):\n    return 1\n"
        );
        assert_eq!(unused_in(&collected), ["parts"]);
        let a_task = format!("{HEAD}@task(inputs={{\"a\": up}})\ndef t(a):\n    return 1\n");
        assert_eq!(unused_in(&a_task), ["a"]);
        // A partition parameter is not an input: never reported, used or not.
        let partitioned = format!(
            "{HEAD}from barca import partitions\n\
             @asset(inputs={{\"a\": up}}, partitions={{\"k\": partitions([\"x\"])}})\n\
             def s(a, k):\n    return a\n"
        );
        assert!(unused_in(&partitioned).is_empty());
    }

    #[test]
    fn a_task_that_only_fans_out_with_parallel_is_judged_like_any_body() {
        let src = format!(
            "{HEAD}from barca import parallel\n@task\ndef child(): return 1\n\
             @task(inputs={{\"a\": up}})\ndef fan(a):\n    return parallel(child)\n"
        );
        assert_eq!(unused_in(&src), ["a"]);
        let used = src.replace("return parallel(child)", "return parallel(child), a");
        assert!(unused_in(&used).is_empty());
    }
}
