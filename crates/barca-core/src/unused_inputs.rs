//! Static check: which declared data inputs does a step's function never use? (#231)
//!
//! Pure function over the function's AST, called by the parser while it already holds the
//! parsed file, so the check costs two walks of each decorated function body that declares
//! data inputs (one for the imports made inside it, one for the uses) and no second parse.
//! Nothing is imported or executed.
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

/// Calls that take a query, an expression or a table name as text and resolve names in it
/// against the caller's variables: DuckDB (`duckdb.sql`, `con.execute`, `duckdb.query`,
/// `duckdb.table("orders")`, `duckdb.view("orders")`), polars (`pl.sql`, `pl.SQLContext`),
/// pandas (`df.query`, `pd.read_sql` on a DuckDB connection; `df.eval` is covered by `eval` in
/// [`DYNAMIC_ACCESS`]). Matched by the called name: as an attribute (`d.sql`, whatever `d` is),
/// bare, or bare under an import alias (`from duckdb import sql as dsql`).
///
/// When every argument of such a call is a literal, the string literals are searched for input
/// names like any other string. When any argument is anything else (a variable, a constant
/// defined elsewhere, an f-string, a concatenation), the text cannot be read here, so the
/// function is never reported. The same holds when an entry point is used as a value instead
/// of being called (`q = duckdb.sql`, `map(con.execute, queries)`): where it is called, and
/// with what, is not followed.
///
/// One case is provably not an entry point: `module.name(...)` where `module` is bound by an
/// import to a module other than [`QUERY_MODULES`] and never rebound (`pa.table(d)`,
/// `np.view(...)`, `json.query`). Every other receiver (a local, a parameter, an attribute
/// chain, a call result) may be a connection, a cursor, a frame or a relation, and silences.
pub const SQL_ENTRY_POINTS: &[&str] = &[
    "sql",
    "execute",
    "executemany",
    "query",
    "from_query",
    "table",
    "view",
    "read_sql",
    "read_sql_query",
    "SQLContext",
];

/// The libraries whose calls can resolve a caller's variable by name.
pub const QUERY_MODULES: &[&str] = &["duckdb", "polars", "pandas"];

/// What a name at the top of the file was imported as, for seeing through aliases.
pub enum Imported<'n> {
    /// `from module import name [as local]`
    Name { module: &'n str, name: &'n str },
    /// `import module [as local]`
    Module(&'n str),
}

impl<'n> Imported<'n> {
    /// (module, name imported from it), the name being `None` for a module import.
    fn parts(&self) -> (&'n str, Option<&'n str>) {
        match *self {
            Imported::Name { module, name } => (module, Some(name)),
            Imported::Module(module) => (module, None),
        }
    }
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
/// - its body calls one of [`SQL_ENTRY_POINTS`] with any argument that is not a literal, or
///   uses one as a value.
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

    let mut scope = Bindings::default();
    scope.visit_parameters(params);
    visitor::walk_body(&mut scope, &func.body);
    let mut uses = Uses {
        candidates: &candidates,
        used: vec![false; candidates.len()],
        dynamic: false,
        imported,
        local: scope.imports,
        rebound: scope.rebound,
        callee: std::ptr::null(),
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

/// What the body binds: the imports made inside it, and every name that is a parameter or is
/// assigned somewhere in it (so an imported module name may not be that module any more).
#[derive(Default)]
struct Bindings<'a> {
    imports: Vec<(&'a str, Imported<'a>)>,
    rebound: Vec<&'a str>,
}

impl<'a> Visitor<'a> for Bindings<'a> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::ImportFrom(import) => {
                let module = import.module.as_ref().map_or("", |m| m.as_str());
                for alias in &import.names {
                    let name = alias.name.as_str();
                    let local = alias.asname.as_ref().map_or(name, |a| a.as_str());
                    self.imports.push((local, Imported::Name { module, name }));
                }
            }
            Stmt::Import(import) => {
                for alias in &import.names {
                    let module = alias.name.as_str();
                    let local = alias.asname.as_ref().map_or(module, |a| a.as_str());
                    self.imports.push((local, Imported::Module(module)));
                }
            }
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Name(n) = expr
            && !n.ctx.is_load()
        {
            self.rebound.push(n.id.as_str());
        }
        visitor::walk_expr(self, expr);
    }

    fn visit_parameter(&mut self, parameter: &'a ast::Parameter) {
        self.rebound.push(parameter.name.as_str());
        visitor::walk_parameter(self, parameter);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.rebound.push(name.as_str());
        }
        visitor::walk_except_handler(self, handler);
    }
}

struct Uses<'c, 'n, 'a> {
    candidates: &'c [&'c str],
    used: Vec<bool>,
    dynamic: bool,
    /// Imports at the top of the file.
    imported: &'n dyn Fn(&str) -> Option<Imported<'n>>,
    /// Imports inside the body; they shadow the file's.
    local: Vec<(&'a str, Imported<'a>)>,
    /// Parameters and names assigned in the body: not reliably what an import bound them to.
    rebound: Vec<&'a str>,
    /// The callee of the call being walked, so that it is not also taken for a value.
    callee: *const Expr,
}

impl Uses<'_, '_, '_> {
    /// The text of one string literal part of the body: every input named in it is mentioned.
    fn text(&mut self, text: &str) {
        for (i, name) in self.candidates.iter().enumerate() {
            if !self.used[i] && mentions_identifier(text, name) {
                self.used[i] = true;
            }
        }
    }

    /// What the local name `local` was imported as: (module, original name) for a
    /// from-import, (module, None) for a module import.
    fn import_of<'s>(&'s self, local: &str) -> Option<(&'s str, Option<&'s str>)> {
        if let Some((_, imported)) = self.local.iter().rev().find(|(l, _)| *l == local) {
            return Some(imported.parts());
        }
        (self.imported)(local).map(|imported| imported.parts())
    }

    /// Whether `expr` refers to one of [`SQL_ENTRY_POINTS`]: `anything.sql`, a from-imported
    /// `sql` under any local name, or (`bare` only, for a callee) an unimported name `sql`.
    /// `module.sql` is not one when `module` is provably a module outside [`QUERY_MODULES`].
    fn is_entry_point(&self, expr: &Expr, bare: bool) -> bool {
        match expr {
            Expr::Name(n) => {
                let name = n.id.as_str();
                match self.import_of(name) {
                    Some((_, Some(original))) if !self.rebound.contains(&name) => {
                        SQL_ENTRY_POINTS.contains(&original)
                    }
                    _ => bare && SQL_ENTRY_POINTS.contains(&name),
                }
            }
            Expr::Attribute(a) => {
                SQL_ENTRY_POINTS.contains(&a.attr.as_str()) && !self.is_other_module(&a.value)
            }
            _ => false,
        }
    }

    /// `receiver` is a name that an import binds to a module outside [`QUERY_MODULES`], and
    /// nothing in the function rebinds it. Anything less certain is `false`.
    fn is_other_module(&self, receiver: &Expr) -> bool {
        let Expr::Name(n) = receiver else {
            return false;
        };
        let name = n.id.as_str();
        match self.import_of(name) {
            Some((module, None)) if !self.rebound.contains(&name) => {
                let root = module.split('.').next().unwrap_or(module);
                !QUERY_MODULES.contains(&root)
            }
            _ => false,
        }
    }

    /// A call of one of [`SQL_ENTRY_POINTS`] with an argument that is not a literal: the
    /// names it reads cannot be known here.
    fn is_unreadable_query(&self, call: &ast::ExprCall) -> bool {
        if !self.is_entry_point(&call.func, true) {
            return false;
        }
        let literal = |e: &Expr| {
            matches!(
                e,
                Expr::StringLiteral(_)
                    | Expr::BytesLiteral(_)
                    | Expr::NumberLiteral(_)
                    | Expr::BooleanLiteral(_)
                    | Expr::NoneLiteral(_)
            )
        };
        let args = &call.arguments;
        !(args.args.iter().all(literal) && args.keywords.iter().all(|k| literal(&k.value)))
    }
}

impl<'a> Visitor<'a> for Uses<'_, '_, 'a> {
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
        // An entry point used as a value (assigned, passed, stored) may be called anywhere
        // with anything. The callee of a call is judged with its arguments instead.
        let is_callee = std::ptr::eq(expr, self.callee);
        if !is_callee && self.is_entry_point(expr, false) {
            self.dynamic = true;
        }
        match expr {
            Expr::Name(n) => {
                let name = n.id.as_str();
                if DYNAMIC_ACCESS.contains(&name) {
                    self.dynamic = true;
                }
                if let Some(i) = self.candidates.iter().position(|c| *c == name) {
                    self.used[i] = true;
                }
                if let Some((module, Some(original))) = self.import_of(name)
                    && (DYNAMIC_ACCESS.contains(&original) || (module, original) == STACK_MODULE)
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
                    let module = match self.import_of(local) {
                        Some((module, None)) => module,
                        _ => local,
                    };
                    if module == STACK_MODULE.0 {
                        self.dynamic = true;
                    }
                }
            }
            Expr::Call(call) => {
                if self.is_unreadable_query(call) {
                    self.dynamic = true;
                }
                // The walk below visits the callee first.
                self.callee = &*call.func;
            }
            _ => {}
        }
        visitor::walk_expr(self, expr);
    }

    // Text is collected where the AST hands out every literal part, however the parts are
    // combined: a plain string, each part of an implicit concatenation (also one that mixes
    // plain strings with f-strings), bytes, and the literal text between the `{...}` of an
    // f-string or t-string (a format spec included). No expression shape is matched.
    fn visit_string_literal(&mut self, literal: &'a ast::StringLiteral) {
        self.text(&literal.value);
    }

    fn visit_bytes_literal(&mut self, literal: &'a ast::BytesLiteral) {
        self.text(&String::from_utf8_lossy(&literal.value));
    }

    fn visit_interpolated_string_element(&mut self, element: &'a ast::InterpolatedStringElement) {
        if let ast::InterpolatedStringElement::Literal(literal) = element {
            self.text(&literal.value);
        }
        // The `{...}` parts are code: walked as expressions (nested strings included).
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
        // Any non-literal argument, in any position or keyword, makes the call unreadable:
        // which argument is the query depends on the callee.
        for body in [
            "    return duckdb.sql(alias=\"x\", query=Q)",
            "    return duckdb.sql(\"select 1\", alias=name)",
            "    return rel.query(\"v\", q)",
            "    return con.execute(\"select * from t where a > ?\", [limit])",
            "    return duckdb.table(name)",
            "    return con.view(name)",
        ] {
            assert!(unused_orders("", body).is_empty(), "{body}");
        }
        // All-literal arguments are read: literals of other types do not hide anything.
        let body =
            "    return duckdb.sql(\"select * from orders\", alias=\"x\", limit=5, flag=True)";
        assert_eq!(unused_orders("", body), ["threshold"]);
        assert_eq!(
            unused_orders("", "    return duckdb.table(\"orders\")"),
            ["threshold"]
        );
    }

    #[test]
    fn an_aliased_entry_point_or_dynamic_name_is_recognised() {
        let with = |imports: &str, body: &str| {
            let src = step(
                "orders, threshold",
                body,
                "\"orders\": up, \"threshold\": up",
            );
            unused_in(&format!("{imports}\n{src}"))
        };
        // Imported at the top of the file ...
        assert!(with("from duckdb import sql as dsql", "    return dsql(q)").is_empty());
        assert!(
            with(
                "from duckdb import query as run_query",
                "    return run_query(q)"
            )
            .is_empty()
        );
        assert!(with("import duckdb as d", "    return d.sql(q)").is_empty());
        // ... or inside the body, before or after the use, also in a nested function.
        for body in [
            "    from duckdb import sql as dsql\n    return dsql(q)",
            "    def inner():\n        return dsql(q)\n    from duckdb import sql as dsql\n    return inner()",
            "    def inner():\n        from duckdb import sql as dsql\n        return dsql(q)\n    return inner()",
            "    import duckdb as d\n    return d.sql(q)",
            "    from inspect import currentframe as cf\n    return cf()",
            "    from inspect import stack as st\n    return st()",
            "    import inspect as i\n    return i.stack()",
        ] {
            assert!(with("", body).is_empty(), "{body}");
        }
        // An aliased entry point with a literal query is read like any other.
        let body = "    return dsql(\"select * from orders\")";
        assert_eq!(with("from duckdb import sql as dsql", body), ["threshold"]);
        // An alias of something else is nothing special.
        assert_eq!(
            with("from mylib import compute as dsql", "    return dsql(q)"),
            ["orders", "threshold"]
        );
    }

    #[test]
    fn an_entry_point_used_as_a_value_is_never_reported() {
        let with = |imports: &str, body: &str| {
            let src = step(
                "orders, threshold",
                body,
                "\"orders\": up, \"threshold\": up",
            );
            unused_in(&format!("{imports}\n{src}"))
        };
        for body in [
            "    q = duckdb.sql\n    return q(QUERY)",
            "    return functools.partial(duckdb.sql, QUERY)()",
            "    return list(map(con.execute, queries))",
            "    runners = [duckdb.sql, con.execute]\n    return runners[0](QUERY)",
            "    return helper(run=duckdb.query)",
            "    return {\"t\": con.table}[kind](name)",
            "    return apply(dsql, QUERY)",
            "    q = dsql\n    return q(QUERY)",
        ] {
            assert!(
                with("from duckdb import sql as dsql", body).is_empty(),
                "{body}"
            );
        }
        // A local variable that merely has such a name is not an entry point used as a value.
        for body in [
            "    query = build()\n    return run(query)",
            "    sql = 1\n    table = 2\n    return sql + table",
        ] {
            assert_eq!(with("", body), ["orders", "threshold"], "{body}");
        }
    }

    #[test]
    fn a_call_on_a_module_that_is_not_duckdb_polars_or_pandas_is_not_an_entry_point() {
        const IMPORTS: &str = "import pyarrow as pa\nimport numpy as np\nimport json\n\
                               import matplotlib.pyplot as plt\nimport duckdb as d\n\
                               import polars\nimport pandas as pd";
        let with = |body: &str| {
            let src = step(
                "orders, threshold",
                body,
                "\"orders\": up, \"threshold\": up",
            );
            unused_in(&format!("{IMPORTS}\n{src}"))
        };
        // Provably another module: reported again.
        for body in [
            "    return pa.table(d)",
            "    arr = np.asarray(x)\n    return np.view(arr, kind)",
            "    return plt.table(cellText=cells)",
            "    return json.query(doc, path)",
            "    f = pa.table\n    return f(data)",
            "    import pyarrow\n    return pyarrow.table(data)",
        ] {
            assert_eq!(with(body), ["orders", "threshold"], "{body}");
        }
        // Not provably so: every doubt stays silent.
        for body in [
            "    return con.table(name)", // a local or global of unknown origin
            "    return cur.execute(q)",
            "    return client.query(q)",
            "    return df.query(expr)",
            "    return rel.view(name)",
            "    return d.sql(q)", // duckdb under an alias
            "    return polars.sql(q)",
            "    return pd.read_sql(q, con)",
            "    return get_con().execute(q)", // a call result
            "    return self.con.execute(q)",  // an attribute chain
            "    return pa.thing.table(name)",
            "    return arr.view(np.int64)", // a local, whatever it holds
            "    pa = connect()\n    return pa.table(name)", // rebound in the body
            "    for np in cons:\n        np.execute(q)\n    return 1",
            "    def inner(json):\n        return json.query(q)\n    return inner(con)",
            "    f = lambda pa: pa.table(name)\n    return f(con)",
            "    with connect() as plt:\n        return plt.table(name)",
        ] {
            assert!(with(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn a_parameter_named_like_an_imported_module_is_not_that_module() {
        let src = "import pyarrow as pa\n".to_string()
            + &step(
                "pa, orders",
                "    return pa.table(name)",
                "\"pa\": up, \"orders\": up",
            );
        assert!(unused_in(&src).is_empty());
    }

    /// Every string shape: text is collected per literal part, however the parts are combined.
    #[test]
    fn every_literal_part_of_any_string_shape_is_searched() {
        for body in [
            // plain + f-string, both orders
            "    return run(\"select sum(amount) from orders \" f\"where amount > {n}\")",
            "    return run(f\"select {col} \" \"from orders\")",
            // the f-string part itself names nothing: only its plain neighbour does
            "    return run(\"from orders \" f\"{n}\")",
            "    return run(f\"{n}\" \" from orders\")",
            // three parts, mixed
            "    return run(\"select * \" f\"from {schema}.t \" \"join orders using (id)\")",
            "    return run(\"select * \" \"from \" \"orders\")",
            "    return run(f\"a {x} \" f\"b {y} \" f\"from orders\")",
            // bytes + bytes
            "    return run(b\"select * \" b\"from orders\")",
            // a string inside an f-string's expression part (nested), and a nested f-string
            "    return run(f\"{lookup('orders')}\")",
            "    return run(f\"{f'from orders {n}'}\")",
            "    return run(f\"{tables['orders']!r:>10}\")",
            // a format spec's literal text
            "    return run(f\"{value:orders}\")",
            "    return run(f\"{value:{width}} orders\")",
            // multi-line concatenation in parentheses, and a raw string
            "    q = (\n        \"select *\"\n        f\" from {schema}.t\"\n        \" join orders\"\n    )\n    return run(q)",
            "    return run(r\"from orders\")",
        ] {
            assert_eq!(unused_orders("", body), ["threshold"], "{body}");
        }
        // The reproductions: through an entry point the mixed concatenation is also a
        // non-literal argument, so nothing at all is reported.
        for body in [
            "    return rel.query(\"v\", \"select sum(amount) from orders \" f\"where amount > {n}\")",
            "    from duckdb import sql as dsql\n    return dsql(\"select 1 from orders \" f\"where a > {n}\")",
        ] {
            assert!(unused_orders("", body).is_empty(), "{body}");
        }
        // Negative: none of these parts names the input.
        for body in [
            "    return run(\"select * \" f\"from reorders {n}\")",
            "    return run(f\"{value:>10}\" \" from customers\")",
            "    return run(b\"from \" b\"orders_v2\")",
        ] {
            assert_eq!(unused_orders("", body), ["orders", "threshold"], "{body}");
        }
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
