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
/// that mentions any of these, as a bare name (also when imported under another name) or as an
/// attribute (`builtins.locals`, `frame.f_locals`, `sys._getframe`), is never reported.
/// `inspect.stack` belongs here too but `stack` alone is too common a name (`np.stack`,
/// `df.stack()`), so it is matched with its module: see [`STACK_MODULE`].
/// `globals` is not here: it cannot expose a parameter.
///
/// This is the one place the lists live; the manual (`assets.md`, "Unused inputs") repeats
/// them and a test keeps them in step.
pub const DYNAMIC_ACCESS: &[&str] = &[
    "locals",
    "vars",
    "eval",
    "exec",
    "currentframe",
    "_getframe",
    "f_locals",
    "f_back",
    "getargvalues",
];

/// `inspect.stack` (as `inspect.stack`, under a module alias, or `from inspect import stack`)
/// is dynamic access like [`DYNAMIC_ACCESS`]: the frames it returns hold the parameters.
pub const STACK_MODULE: (&str, &str) = ("inspect", "stack");

/// Calls that take a query or an expression as text and resolve names in it against the
/// caller's variables: DuckDB (`duckdb.sql`, `con.execute`, `duckdb.query`), polars
/// (`pl.sql`, `pl.SQLContext`), pandas (`df.query`, `pd.read_sql` on a DuckDB connection;
/// `df.eval` is covered by `eval` in [`DYNAMIC_ACCESS`]). Matched by the called name, bare or
/// as an attribute. When such a call's query is a plain string literal, the literal itself is
/// searched for input names. When it is anything else (a variable, a constant defined
/// elsewhere, an f-string, a concatenation), the text cannot be read here, so the function is
/// never reported.
pub const SQL_ENTRY_POINTS: &[&str] = &[
    "sql",
    "execute",
    "executemany",
    "query",
    "from_query",
    "read_sql",
    "read_sql_query",
    "SQLContext",
];

/// What a name at the top of the file was imported as, for seeing through aliases.
pub enum Imported<'n> {
    /// `from module import name [as local]`
    Name { module: &'n str, name: &'n str },
    /// `import module [as local]`
    Module(&'n str),
}

/// The data inputs of `func` that its body never uses, in declaration order.
///
/// A data input is a parameter wired by `inputs=` whose name does not start with `_`
/// (`_`-prefixed inputs are ordering-only: declared unused on purpose, never reported).
/// It is unused when the body never mentions its name, or mentions it only as the target of
/// `del name`. A mention is
/// - the name in code, in any position and any nested scope: reading it, passing it to a
///   helper, a closure, a comprehension, an f-string, assigning to it;
/// - the name as a whole identifier inside any string or bytes literal of the body, f-string
///   text included (`"select * from orders"`, `"amount > @threshold"`): SQL and expression
///   strings read variables by name. `orders` does not match `reorders` or `orders_v2`, and
///   the match is case-sensitive. A string that is a statement on its own (a docstring) does
///   not count: describing a parameter is not using it. Comments are not part of the AST.
///
/// An input annotated `duckdb.DuckDBPyRelation` is never reported: barca binds it as a view
/// named after the parameter, so SQL in a helper function, which this check does not read,
/// can use it without the body naming it at all.
///
/// Nothing is reported for the whole function when
/// - it takes `**kwargs` (inputs are passed by keyword, so they can arrive there),
/// - its body has no real statement: only a docstring, `pass`, `...` or `raise` (a stub, or a
///   gate that only raises, uses nothing by definition),
/// - its body mentions one of [`DYNAMIC_ACCESS`] or `inspect.stack`,
/// - its body calls one of [`SQL_ENTRY_POINTS`] with a query that is not a plain string
///   literal.
///
/// `imported` says what a top-level name of the file was imported as, so
/// `from inspect import currentframe as cf` and `import inspect as i` are seen.
pub fn unused_inputs<'n>(
    func: &ast::StmtFunctionDef,
    inputs: &[DeclaredInput],
    param_types: &HashMap<String, ValueType>,
    imported: &'n dyn Fn(&str) -> Option<Imported<'n>>,
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
        imported,
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

/// Whether `name` occurs in `text` as a whole identifier: not preceded or followed by a
/// letter, a digit or `_`. (`@name`, pandas' spelling of a local, matches: `@` is neither.)
fn mentions_identifier(text: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(is_ident)
            && !text[at + name.len()..].chars().next().is_some_and(is_ident)
    })
}

struct Uses<'c, 'n> {
    candidates: &'c [&'c str],
    used: Vec<bool>,
    dynamic: bool,
    imported: &'n dyn Fn(&str) -> Option<Imported<'n>>,
}

impl Uses<'_, '_> {
    /// The text of a string literal in the body: every input named in it is mentioned.
    fn text(&mut self, text: &str) {
        for (i, name) in self.candidates.iter().enumerate() {
            if !self.used[i] && mentions_identifier(text, name) {
                self.used[i] = true;
            }
        }
    }

    /// A call of one of [`SQL_ENTRY_POINTS`] whose query (the first argument) is not a plain
    /// string literal: the names it reads cannot be known here.
    fn is_unreadable_query(call: &ast::ExprCall) -> bool {
        let callee = match &*call.func {
            Expr::Name(n) => n.id.as_str(),
            Expr::Attribute(a) => a.attr.as_str(),
            _ => return false,
        };
        if !SQL_ENTRY_POINTS.contains(&callee) {
            return false;
        }
        let query = call
            .arguments
            .args
            .first()
            .or_else(|| call.arguments.keywords.first().map(|k| &k.value));
        query.is_some_and(|q| !matches!(q, Expr::StringLiteral(_) | Expr::BytesLiteral(_)))
    }
}

impl<'a> Visitor<'a> for Uses<'_, '_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::Delete(del) => {
                // `del df` is not a use; `del df[0]` and `del df.x` read `df`.
                for target in &del.targets {
                    if !matches!(target, Expr::Name(_)) {
                        self.visit_expr(target);
                    }
                }
            }
            // A string that is a whole statement does nothing: a docstring (of the step or of
            // a nested function), which may describe an input without using it.
            Stmt::Expr(e) if matches!(&*e.value, Expr::StringLiteral(_)) => {}
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(n) => {
                let name = n.id.as_str();
                if DYNAMIC_ACCESS.contains(&name) {
                    self.dynamic = true;
                }
                if let Some(i) = self.candidates.iter().position(|c| *c == name) {
                    self.used[i] = true;
                }
                if let Some(Imported::Name { module, name }) = (self.imported)(name)
                    && (DYNAMIC_ACCESS.contains(&name) || (module, name) == STACK_MODULE)
                {
                    self.dynamic = true;
                }
            }
            Expr::Attribute(a) => {
                let attr = a.attr.as_str();
                if DYNAMIC_ACCESS.contains(&attr) {
                    self.dynamic = true;
                }
                if attr == STACK_MODULE.1
                    && let Expr::Name(receiver) = &*a.value
                {
                    let local = receiver.id.as_str();
                    let module = match (self.imported)(local) {
                        Some(Imported::Module(module)) => module,
                        _ => local,
                    };
                    if module == STACK_MODULE.0 {
                        self.dynamic = true;
                    }
                }
            }
            Expr::Call(call) if Self::is_unreadable_query(call) => self.dynamic = true,
            Expr::StringLiteral(s) => self.text(s.value.to_str()),
            Expr::BytesLiteral(b) => {
                let bytes: Vec<u8> = b.value.bytes().collect();
                self.text(&String::from_utf8_lossy(&bytes));
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }

    fn visit_interpolated_string_element(&mut self, element: &'a ast::InterpolatedStringElement) {
        // The literal text of an f-string (or t-string); its `{...}` parts are walked as code.
        if let ast::InterpolatedStringElement::Literal(literal) = element {
            self.text(&literal.value);
        }
        visitor::walk_interpolated_string_element(self, element);
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

    /// The unused inputs of a step `s(orders, threshold)` with this body.
    fn unused_orders(annotation: &str, body: &str) -> Vec<String> {
        unused_in(&step(
            &format!("orders{annotation}, threshold"),
            body,
            "\"orders\": up, \"threshold\": up",
        ))
    }

    #[test]
    fn a_name_inside_a_string_literal_is_a_mention() {
        // DuckDB replacement scans and polars SQL resolve local variables by name; pandas
        // `query` reads `@name`. Each of these ran and returned the right value while warning.
        for (annotation, body, unused) in [
            (
                ": pd.DataFrame",
                "    return duckdb.sql(\"select sum(amount) from orders\").fetchone()",
                "threshold",
            ),
            (
                ": pl.DataFrame",
                "    return duckdb.sql(\"select sum(amount) from orders\").fetchone()",
                "threshold",
            ),
            (
                "",
                "    return duckdb.sql(\"select sum(amount) from orders\").fetchone()",
                "threshold",
            ),
            (
                ": pl.LazyFrame",
                "    return pl.sql(\"select sum(amount) from orders\").collect()",
                "threshold",
            ),
            (
                "",
                "    return other.query(\"amount > @threshold['min']\")",
                "orders",
            ),
            // multi-line, concatenated, f-string text, bytes, any call or none at all
            (
                "",
                "    q = \"\"\"\n    select *\n    from orders\n    \"\"\"\n    return run(q)",
                "threshold",
            ),
            (
                "",
                "    return run(\"select * from \" \"orders\")",
                "threshold",
            ),
            (
                "",
                "    return run(\"select * \" + \"from orders\")",
                "threshold",
            ),
            (
                "",
                "    return run(f\"select * from orders limit {n}\")",
                "threshold",
            ),
            ("", "    return run(b\"select * from orders\")", "threshold"),
            ("", "    return {\"orders\": 1}", "threshold"),
            (
                "",
                "    return run(\"select o.x from orders o join t on o.id=t.id\")",
                "threshold",
            ),
            (
                "",
                "    return run(\"from 'orders'\"), run('(orders)'), run(\"x,orders;\")",
                "threshold",
            ),
        ] {
            assert_eq!(
                unused_orders(annotation, body),
                [unused],
                "{annotation} {body}"
            );
        }
        // Both named in strings: nothing to report.
        let body = "    return duckdb.sql(\"from orders where amount > 1\").df().query(\"a > @threshold\")";
        assert!(unused_orders("", body).is_empty());
    }

    #[test]
    fn the_string_rule_matches_whole_identifiers_only() {
        for body in [
            "    return run(\"select * from reorders\")", // substring, prefix side
            "    return run(\"select * from orders_v2\")", // substring, suffix side
            "    return run(\"select * from orders2\")",
            "    return run(\"select * from Orders\")", // case-sensitive
            "    return run(\"select * from customers\")", // a different name
            "    return run(f\"select * from reorders {n}\")",
        ] {
            assert_eq!(unused_orders("", body), ["orders", "threshold"], "{body}");
        }
    }

    #[test]
    fn a_docstring_or_a_comment_naming_the_input_is_not_a_use() {
        for body in [
            "    \"\"\"Summarise orders above threshold.\"\"\"\n    return 1",
            "    # reads orders and threshold\n    return 1",
            "    return 1  # orders, threshold",
            "    def inner():\n        \"\"\"Uses orders and threshold.\"\"\"\n        return 1\n    return inner()",
            "    x = 1\n    \"orders and threshold, as a stray string statement\"\n    return x",
        ] {
            assert_eq!(unused_orders("", body), ["orders", "threshold"], "{body}");
        }
        // The docstring does not hide a real mention after it.
        let body = "    \"\"\"Doc.\"\"\"\n    return run(\"from orders\")";
        assert_eq!(unused_orders("", body), ["threshold"]);
    }

    /// One case per entry of SQL_ENTRY_POINTS: the query is not a literal, so barca cannot
    /// tell which inputs it reads and reports nothing.
    #[test]
    fn a_query_built_elsewhere_is_never_reported() {
        for body in [
            "    return duckdb.sql(QUERY)",
            "    return duckdb.sql(query=QUERY)",
            "    return con.execute(q).fetchall()",
            "    return con.executemany(q, rows)",
            "    return pl.sql(q).collect()",
            "    return pl.SQLContext(frames).execute(\"select 1\")",
            "    return other.query(expr)",
            "    return duckdb.query(build())",
            "    return duckdb.from_query(q)",
            "    return pd.read_sql(q, con)",
            "    return pd.read_sql_query(q, con)",
            "    return other.eval(expr)",
            "    return duckdb.sql(f\"select * from {table}\")",
            "    return duckdb.sql(\"select * from \" + table)",
            "    return sql(q)",
        ] {
            assert!(unused_orders("", body).is_empty(), "{body}");
        }
        for name in super::SQL_ENTRY_POINTS {
            assert!(
                unused_orders("", &format!("    return x.{name}(q)")).is_empty(),
                "{name}"
            );
            // A literal query is read instead: it names neither input here.
            assert_eq!(
                unused_orders("", &format!("    return x.{name}(\"select 1\")")),
                ["orders", "threshold"],
                "{name}"
            );
        }
        // A non-literal second argument (bind parameters) does not make the query unreadable.
        let body = "    return con.execute(\"select * from orders where a > ?\", [limit])";
        assert_eq!(unused_orders("", body), ["threshold"]);
    }

    #[test]
    fn inspect_stack_is_dynamic_access_but_other_stacks_are_not() {
        for body in [
            "    return inspect.stack()[0].frame",
            "    return i.stack()",
            "    return stack()[0]",
            "    return helper(inspect.stack)",
        ] {
            let src = format!(
                "import inspect as i\nfrom inspect import stack\n{}",
                step("a", body, "\"a\": up")
            );
            assert!(unused_in(&src).is_empty(), "{body}");
        }
        for body in [
            "    return np.stack(parts)",
            "    return frame.stack()",
            "    return stack()",
        ] {
            assert_eq!(unused("a", body), ["a"], "{body}");
        }
    }

    #[test]
    fn globals_cannot_reach_a_parameter_so_it_does_not_silence() {
        assert_eq!(unused("a", "    return globals()"), ["a"]);
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
        // Bound as a view named `a`: SQL in a helper reads it with no mention in the body at
        // all, which the string rule cannot see. That is why the annotation is exempt.
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
