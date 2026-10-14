//! Which arguments barca's decorators and helpers take, and the plan-time check that a
//! pipeline passes no others (#284).
//!
//! An argument barca does not define used to be ignored: `@asset(after=other)` or a misspelt
//! `input=` planned and ran with exit 0 and did nothing. [`SIGNATURES`] is the one list of what
//! each call accepts. The check below reads it, the tests at the bottom hold the Python stubs
//! (`python/barca/__init__.py`) and the tables in the manual and on the site to it.
//!
//! The check works on the decorators of one function, already parsed: no second parse.
//!
//! Two rules keep it from rejecting a pipeline that worked:
//!
//! - **Only arguments that had no effect are rejected.** Every keyword the parser reads for a
//!   call is in [`SIGNATURES`] (`partitions=` and `serializer=` work on `@task` and `@sensor`
//!   as they do on `@asset`).
//! - **Only names that are positively barca's are checked** ([`BarcaNames`]): bound by a
//!   `from barca import ...` at the top of the module and bound by nothing else. A `task` that
//!   is Celery's, Prefect's or the file's own is not barca's to judge.
//!
//! # Sharing the list
//!
//! [`SIGNATURES`] is the single list of argument names; anything else that needs them (which
//! arguments count toward a definition hash, for instance) should be keyed to it rather than
//! repeat the names. [`arguments`] gives it flat, as `(call, argument, how it is passed)`,
//! and a test on the other table can assert that it names exactly these.

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{self as ast, Expr};
use ruff_text_size::Ranged;

/// What one barca call accepts.
pub struct Signature {
    /// The name as written in a pipeline: `asset`, `partitions`, `Schedule`.
    pub name: &'static str,
    /// Whether it is written with `@`.
    pub decorator: bool,
    /// The arguments it takes by position, by the name the Python signature gives them
    /// (they are positional-only there). The node decorators take none; the others take
    /// exactly these.
    pub positional: &'static [&'static str],
    /// The keyword arguments it accepts, in the order of the Python signature.
    pub keywords: &'static [&'static str],
    /// Keywords that are not accepted but are rejected later with a message of their own.
    deferred: &'static [&'static str],
    /// The call written out, for the fix.
    pub usage: &'static str,
    /// The manual topic that describes it.
    pub topic: &'static str,
}

/// Every barca call whose arguments are read from the source. `@unsafe` takes no call, and
/// `parallel()` / `parallel_map()` run inside a task body: their arguments are yours.
pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "group",
        decorator: false,
        positional: &["name"],
        keywords: &["members", "output", "description"],
        deferred: &[],
        usage: "group(\"training\", members=[features, model], output=model)",
        topic: "groups",
    },
    Signature {
        name: "asset",
        decorator: true,
        positional: &[],
        keywords: &[
            "name",
            "inputs",
            "partitions",
            "serializer",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        deferred: &[],
        usage: "@asset(inputs={\"param\": upstream}, ...)",
        topic: "assets",
    },
    Signature {
        name: "sensor",
        decorator: true,
        positional: &[],
        keywords: &[
            "name",
            "partitions",
            "serializer",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        // "sensor '...' cannot have inputs", when the DAG is built.
        deferred: &["inputs"],
        usage: "@sensor(freshness=Schedule(\"<cron>\"), ...)",
        topic: "assets",
    },
    Signature {
        name: "task",
        decorator: true,
        positional: &[],
        keywords: &[
            "name",
            "inputs",
            "partitions",
            "serializer",
            "freshness",
            "timeout_seconds",
            "retries",
            "retry_backoff",
            "description",
            "tags",
            "env",
        ],
        deferred: &[],
        usage: "@task(inputs={\"param\": upstream}, ...)",
        topic: "tasks",
    },
    Signature {
        name: "sink",
        decorator: true,
        positional: &["path"],
        keywords: &["serializer"],
        deferred: &[],
        usage: "@sink(\"path/to/file.json\", serializer=\"json\")",
        topic: "sinks",
    },
    Signature {
        name: "partitions",
        decorator: false,
        positional: &["values"],
        keywords: &[],
        deferred: &[],
        usage: "partitions([\"a\", \"b\"])",
        topic: "partitions",
    },
    Signature {
        name: "partitions_from",
        decorator: false,
        positional: &["source"],
        keywords: &[],
        deferred: &[],
        usage: "partitions_from(upstream)",
        topic: "partitions",
    },
    Signature {
        name: "collect",
        decorator: false,
        positional: &["asset_fn"],
        keywords: &[],
        deferred: &[],
        usage: "collect(upstream)",
        topic: "partitions",
    },
    Signature {
        name: "asset_ref",
        decorator: false,
        positional: &["ref_string"],
        keywords: &[],
        deferred: &[],
        usage: "asset_ref(\"file.py:name\")",
        topic: "assets",
    },
    Signature {
        name: "Schedule",
        decorator: false,
        positional: &["cron"],
        keywords: &[],
        deferred: &[],
        usage: "Schedule(\"0 5 * * *\")",
        topic: "scheduling",
    },
];

/// How an argument is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Passing {
    /// By position only (`partitions(values)`, the path of `@sink`).
    Positional,
    /// By keyword only (`inputs=`, `serializer=`).
    Keyword,
}

/// One argument of one barca call: the flat form of [`SIGNATURES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argument {
    /// The call as written in a pipeline: `asset`, `sink`, `partitions`, `Schedule`.
    pub call: &'static str,
    /// The argument's name in the Python signature.
    pub name: &'static str,
    pub passing: Passing,
}

/// Every argument of every barca call, in the order of [`SIGNATURES`]: positional arguments
/// first, then keywords in signature order. There is no "any other argument": a call with one
/// is rejected before anything else reads it.
pub fn arguments() -> impl Iterator<Item = Argument> {
    SIGNATURES.iter().flat_map(|sig| {
        let positional = sig.positional.iter().map(move |name| Argument {
            call: sig.name,
            name,
            passing: Passing::Positional,
        });
        let keywords = sig.keywords.iter().map(move |name| Argument {
            call: sig.name,
            name,
            passing: Passing::Keyword,
        });
        positional.chain(keywords)
    })
}

impl Signature {
    pub fn named(name: &str) -> Option<&'static Signature> {
        SIGNATURES.iter().find(|s| s.name == name)
    }

    /// `@asset` or `partitions()`.
    pub fn display(&self) -> String {
        if self.decorator {
            format!("@{}", self.name)
        } else {
            format!("{}()", self.name)
        }
    }

    /// "`@asset` accepts: `name`, `inputs`" or "`collect()` takes no keyword arguments".
    pub fn accepts(&self) -> String {
        if self.keywords.is_empty() {
            format!("{} takes no keyword arguments", self.display())
        } else {
            format!("{} accepts: {}", self.display(), self.keywords.join(", "))
        }
    }

    /// The row of this call in the tables of the manual and the site.
    pub fn doc_row(&self) -> String {
        let keywords = if self.keywords.is_empty() {
            "none".to_string()
        } else {
            self.keywords
                .iter()
                .map(|k| format!("`{k}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let positional = match self.positional.len() {
            0 => "none",
            _ => "one",
        };
        format!("| `{}` | {positional} | {keywords} |", self.display())
    }

    /// The accepted keyword the user most likely meant, when exactly one is close enough for
    /// the guess to be safe: one edit away, or two for a name longer than four characters
    /// (`input` -> `inputs`, `serialiser` -> `serializer`). Two candidates give no suggestion.
    pub fn closest(&self, unknown: &str) -> Option<&'static str> {
        let limit = if unknown.chars().count() > 4 { 2 } else { 1 };
        let mut close = self
            .keywords
            .iter()
            .filter(|k| edit_distance(unknown, k) <= limit);
        match (close.next(), close.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }
}

/// Optimal string alignment distance: insertions, deletions, substitutions and swaps of two
/// neighbouring characters each count one.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    d[0] = (0..=b.len()).collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// One argument a barca call does not take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Byte offset of the argument in the source, for the line number.
    pub offset: usize,
    /// What is wrong, ending with what the call accepts.
    pub message: String,
    /// What to do.
    pub fix: String,
}

pub(crate) fn check_call(call: &ast::ExprCall, sig: &Signature) -> Option<Problem> {
    let shown = sig.display();
    let see = format!("See `barca docs {}`.", sig.topic);
    let args = &call.arguments.args;
    let starred = args.iter().find(|a| matches!(a, Expr::Starred(_)));

    if sig.decorator
        && let Some(star) = starred
    {
        return Some(Problem {
            offset: star.range().start().to_usize(),
            message: format!(
                "{shown} is called with `*` arguments. barca reads decorator arguments from \
                 the source without running it, so it cannot see what they are"
            ),
            fix: format!("Write the arguments out, like `{}`. {see}", sig.usage),
        });
    }
    // How many positional arguments. A helper called with `*values` is left alone: what it
    // receives is not in the source.
    if starred.is_none() && args.len() != sig.positional.len() {
        let takes = match sig.positional {
            [] => "takes keyword arguments only".to_string(),
            [only] => format!("takes one positional argument (`{only}`)"),
            many => format!("takes {} positional arguments", many.len()),
        };
        let given = match args.len() {
            0 => "none".to_string(),
            1 => "one".to_string(),
            n => n.to_string(),
        };
        let by_keyword = sig
            .positional
            .first()
            .filter(|_| args.is_empty())
            .and_then(|name| {
                call.arguments
                    .keywords
                    .iter()
                    .find(|kw| kw.arg.as_ref().is_some_and(|a| a.as_str() == *name))
                    .map(|kw| (*name, kw.range().start().to_usize()))
            });
        if let Some((name, offset)) = by_keyword {
            // `@sink(path="out.json")`, `partitions(values=[...])`: the value is there, under
            // the name the signature gives the positional argument.
            return Some(Problem {
                offset,
                message: format!(
                    "`{name}` is passed by keyword to {shown}, which takes it by position \
                     only. {}",
                    sig.accepts()
                ),
                fix: format!(
                    "Pass the value as the first argument, without `{name}=`, like `{}`. {see}",
                    sig.usage
                ),
            });
        }
        let offset = args
            .get(sig.positional.len())
            .map(|extra| extra.range().start())
            .unwrap_or(call.range().start())
            .to_usize();
        let fix = if args.len() < sig.positional.len() {
            format!("Write it like `{}`. {see}", sig.usage)
        } else if sig.keywords.is_empty() {
            format!(
                "Remove the extra argument: write it like `{}`. {see}",
                sig.usage
            )
        } else {
            format!(
                "Pass the extra argument by keyword, like `{}`, or remove it. {see}",
                sig.usage
            )
        };
        return Some(Problem {
            offset,
            message: format!(
                "{shown} {takes}, and is called with {given}. {}",
                sig.accepts()
            ),
            fix,
        });
    }

    for kw in call.arguments.keywords.iter() {
        let offset = kw.range().start().to_usize();
        let Some(name) = kw.arg.as_ref().map(|a| a.as_str()) else {
            if !sig.decorator {
                continue; // a helper's `**options`: not in the source, left alone
            }
            return Some(Problem {
                offset,
                message: format!(
                    "{shown} is called with `**` arguments. barca reads decorator arguments \
                     from the source without running it, so it cannot see what they are. {}",
                    sig.accepts()
                ),
                fix: format!("Write the arguments out, like `{}`. {see}", sig.usage),
            });
        };
        if sig.keywords.contains(&name) || sig.deferred.contains(&name) {
            continue;
        }
        if sig.positional.contains(&name) {
            // The positional argument given twice: by position and by its name.
            return Some(Problem {
                offset,
                message: format!(
                    "`{name}` is passed by keyword to {shown}, which takes it by position \
                     only. {}",
                    sig.accepts()
                ),
                fix: format!(
                    "Remove `{name}=...`: the first argument is already `{name}`. Write it \
                     like `{}`. {see}",
                    sig.usage
                ),
            });
        }
        let guess = sig.closest(name);
        let meant = guess
            .map(|g| format!(" Did you mean `{g}`?"))
            .unwrap_or_default();
        let fix = match guess {
            Some(g) => format!("Rename `{name}` to `{g}`, or remove it. {see}"),
            None if sig.keywords.is_empty() => {
                format!("Remove `{name}`: write it like `{}`. {see}", sig.usage)
            }
            None => {
                format!("Remove `{name}`, or replace it with an argument {shown} accepts. {see}")
            }
        };
        return Some(Problem {
            offset,
            message: format!(
                "`{name}` is not an argument of {shown}.{meant} {}",
                sig.accepts()
            ),
            fix,
        });
    }
    None
}

/// Conservative static provenance for top-level Barca imports, including aliases.
/// Assignments, competing imports, globals, namespace reflection and writes to module
/// attributes invalidate provenance. No Python module is imported or executed.
/// Explicit competing bindings also prevent legacy bare-name discovery. Reflection alone
/// retains discovery candidates, while hashing and argument validation stay conservative.
#[derive(Debug, Default, Clone)]
pub struct BarcaNames {
    proven: std::collections::HashMap<String, &'static str>,
    candidates: std::collections::HashMap<String, &'static str>,
    bound: std::collections::HashSet<String>,
    /// Explicit writes affect the shared module object, regardless of import spelling.
    mutated_exports: std::collections::HashSet<&'static str>,
}

fn exported_names() -> impl Iterator<Item = &'static str> {
    SIGNATURES.iter().map(|s| s.name).chain([
        "unsafe",
        "Always",
        "Manual",
        "parallel",
        "parallel_map",
    ])
}
fn exported(name: &str) -> Option<&'static str> {
    exported_names().find(|n| *n == name)
}

impl BarcaNames {
    pub fn contains(&self, name: &str) -> bool {
        self.proven.contains_key(name)
    }

    /// A positively imported Barca export, with qualified/aliased spelling resolved.
    pub(crate) fn resolve(&self, expr: &Expr) -> Option<&'static str> {
        match expr {
            Expr::Name(n) => self
                .proven
                .get(n.id.as_str())
                .copied()
                .filter(|n| *n != "module"),
            Expr::Attribute(a) => {
                let Expr::Name(n) = a.value.as_ref() else {
                    return None;
                };
                (self.proven.get(n.id.as_str()) == Some(&"module"))
                    .then(|| exported(a.attr.as_str()))
                    .flatten()
                    .filter(|name| !self.mutated_exports.contains(name))
            }
            _ => None,
        }
    }

    /// Compatibility for bare names in source snippets with no competing binding.
    pub(crate) fn recognized(&self, expr: &Expr) -> Option<&'static str> {
        self.resolve(expr)
            .or_else(|| match expr {
                Expr::Name(n) => self
                    .candidates
                    .get(n.id.as_str())
                    .copied()
                    .filter(|n| *n != "module"),
                Expr::Attribute(a) => {
                    let Expr::Name(n) = a.value.as_ref() else {
                        return None;
                    };
                    (self.candidates.get(n.id.as_str()) == Some(&"module"))
                        .then(|| exported(a.attr.as_str()))
                        .flatten()
                        .filter(|name| !self.mutated_exports.contains(name))
                }
                _ => None,
            })
            .or_else(|| {
                let Expr::Name(n) = expr else { return None };
                (!self.bound.contains(n.id.as_str()))
                    .then(|| exported(n.id.as_str()))
                    .flatten()
            })
    }

    /// Function-local stores/parameters/imports cannot stand for module exports.
    pub(crate) fn in_function(&self, func: &ast::StmtFunctionDef) -> Self {
        struct Locals(std::collections::HashSet<String>);
        impl<'a> Visitor<'a> for Locals {
            fn visit_parameter(&mut self, p: &'a ast::Parameter) {
                self.0.insert(p.name.to_string());
            }
            fn visit_expr(&mut self, e: &'a Expr) {
                if let Expr::Name(n) = e
                    && !n.ctx.is_load()
                {
                    self.0.insert(n.id.to_string());
                }
                visitor::walk_expr(self, e);
            }
            fn visit_stmt(&mut self, s: &'a ast::Stmt) {
                match s {
                    ast::Stmt::FunctionDef(f) => {
                        self.0.insert(f.name.to_string());
                        return;
                    }
                    ast::Stmt::ClassDef(c) => {
                        self.0.insert(c.name.to_string());
                        return;
                    }
                    ast::Stmt::Import(i) => {
                        for a in &i.names {
                            self.0.insert(
                                a.asname
                                    .as_ref()
                                    .map_or_else(
                                        || a.name.as_str().split('.').next().unwrap_or(""),
                                        |n| n.as_str(),
                                    )
                                    .to_string(),
                            );
                        }
                    }
                    ast::Stmt::ImportFrom(i) => {
                        for a in &i.names {
                            self.0.insert(
                                a.asname
                                    .as_ref()
                                    .map_or(a.name.as_str(), |n| n.as_str())
                                    .to_string(),
                            );
                        }
                    }
                    _ => {}
                }
                visitor::walk_stmt(self, s);
            }
            fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
                let ast::ExceptHandler::ExceptHandler(h) = handler;
                if let Some(n) = &h.name {
                    self.0.insert(n.to_string());
                }
                visitor::walk_except_handler(self, handler);
            }
            fn visit_pattern(&mut self, p: &'a ast::Pattern) {
                match p {
                    ast::Pattern::MatchAs(p) => {
                        if let Some(n) = &p.name {
                            self.0.insert(n.to_string());
                        }
                    }
                    ast::Pattern::MatchStar(p) => {
                        if let Some(n) = &p.name {
                            self.0.insert(n.to_string());
                        }
                    }
                    ast::Pattern::MatchMapping(p) => {
                        if let Some(n) = &p.rest {
                            self.0.insert(n.to_string());
                        }
                    }
                    _ => {}
                }
                visitor::walk_pattern(self, p);
            }
        }
        let mut locals = Locals(Default::default());
        locals.visit_parameters(&func.parameters);
        visitor::walk_body(&mut locals, &func.body);
        let mut scoped = self.clone();
        scoped.proven.retain(|n, _| !locals.0.contains(n));
        scoped.candidates.retain(|n, _| !locals.0.contains(n));
        scoped.bound.extend(locals.0);
        scoped
    }

    pub fn of(body: &[ast::Stmt]) -> Self {
        use ast::Stmt;
        use std::collections::{HashMap, HashSet};

        let mut imported: HashMap<String, (&'static str, usize)> = HashMap::new();
        let mut top_level_imports: HashSet<usize> = HashSet::new();
        let mut module_aliases = HashSet::new();
        let mut conflicting = HashSet::new();
        let mut record = |local: String, canonical: &'static str, at: usize| {
            if imported
                .get(&local)
                .is_some_and(|(old, _)| *old != canonical)
            {
                conflicting.insert(local.clone());
            }
            imported.insert(local, (canonical, at));
        };
        for stmt in body {
            let at = stmt.range().start().to_usize();
            match stmt {
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        if alias.name.as_str() == "barca" {
                            top_level_imports.insert(at);
                            let local = alias
                                .asname
                                .as_ref()
                                .map_or("barca", |n| n.as_str())
                                .to_string();
                            module_aliases.insert(local.clone());
                            record(local, "module", at);
                        }
                    }
                }
                Stmt::ImportFrom(imp)
                    if imp.level == 0
                        && imp.module.as_ref().is_some_and(|m| m.as_str() == "barca") =>
                {
                    top_level_imports.insert(at);
                    for alias in &imp.names {
                        if alias.name.as_str() == "*" {
                            for name in exported_names() {
                                record(name.to_string(), name, at);
                            }
                        } else if let Some(name) = exported(alias.name.as_str()) {
                            let local = alias
                                .asname
                                .as_ref()
                                .map_or(alias.name.as_str(), |n| n.as_str());
                            record(local.to_string(), name, at);
                        }
                    }
                }
                _ => {}
            }
        }

        /// Everything else that binds a name at module scope.
        struct Bindings<'s> {
            top_level_imports: &'s HashSet<usize>,
            module_aliases: HashSet<String>,
            mutated_exports: HashSet<&'static str>,
            /// Inside a `def` or `class`: only `global` reaches the module scope from there.
            nested: usize,
            rebound: HashSet<String>,
            /// Offsets of `from <not barca> import *`.
            foreign_stars: Vec<usize>,
            reflective_namespace: bool,
        }
        impl Bindings<'_> {
            fn bind(&mut self, name: &str) {
                if self.nested == 0 {
                    self.rebound.insert(name.to_string());
                }
            }
            fn bind_target(&mut self, target: &Expr) {
                match target {
                    Expr::Name(n) => self.bind(n.id.as_str()),
                    Expr::Tuple(t) => t.elts.iter().for_each(|e| self.bind_target(e)),
                    Expr::List(l) => l.elts.iter().for_each(|e| self.bind_target(e)),
                    Expr::Starred(s) => self.bind_target(&s.value),
                    Expr::Attribute(a) => {
                        // A known export is shared by every module alias and direct import.
                        if let Expr::Name(n) = a.value.as_ref() {
                            if self.module_aliases.contains(n.id.as_str())
                                && let Some(export) = exported(a.attr.as_str())
                            {
                                self.mutated_exports.insert(export);
                            } else {
                                self.rebound.insert(n.id.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        impl<'a> Visitor<'a> for Bindings<'_> {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                if let Stmt::ImportFrom(import) = stmt
                    && import.names.iter().any(|alias| {
                        matches!(
                            alias.name.as_str(),
                            "globals" | "locals" | "vars" | "exec" | "eval" | "__builtins__"
                        )
                    })
                {
                    self.reflective_namespace = true;
                }
                match stmt {
                    Stmt::FunctionDef(f) => {
                        self.bind(f.name.as_str());
                        self.nested += 1;
                        visitor::walk_stmt(self, stmt);
                        self.nested -= 1;
                        return;
                    }
                    Stmt::ClassDef(c) => {
                        self.bind(c.name.as_str());
                        self.nested += 1;
                        visitor::walk_stmt(self, stmt);
                        self.nested -= 1;
                        return;
                    }
                    Stmt::Global(g) => {
                        for name in &g.names {
                            self.rebound.insert(name.to_string());
                        }
                    }
                    Stmt::Assign(a) => a.targets.iter().for_each(|t| self.bind_target(t)),
                    Stmt::AnnAssign(a) => self.bind_target(&a.target),
                    Stmt::AugAssign(a) => self.bind_target(&a.target),
                    Stmt::TypeAlias(t) => self.bind_target(&t.name),
                    Stmt::Delete(d) => d.targets.iter().for_each(|t| self.bind_target(t)),
                    Stmt::For(f) => self.bind_target(&f.target),
                    Stmt::With(w) => {
                        for item in &w.items {
                            if let Some(vars) = &item.optional_vars {
                                self.bind_target(vars);
                            }
                        }
                    }
                    Stmt::Import(imp) => {
                        for alias in &imp.names {
                            let bound = match &alias.asname {
                                Some(asname) => asname.as_str(),
                                None => alias.name.as_str().split('.').next().unwrap_or(""),
                            };
                            if !self
                                .top_level_imports
                                .contains(&imp.range().start().to_usize())
                                || alias.name.as_str() != "barca"
                            {
                                self.bind(bound);
                            }
                        }
                    }
                    Stmt::ImportFrom(imp) => {
                        let at = imp.range().start().to_usize();
                        let positive = self.top_level_imports.contains(&at);
                        for alias in &imp.names {
                            if alias.name.as_str() == "*" {
                                if !positive && self.nested == 0 {
                                    self.foreign_stars.push(at);
                                }
                            } else if (!positive || exported(alias.name.as_str()).is_none())
                                && let Some(asname) = &alias.asname
                            {
                                // `from barca import task as asset` makes `asset` a task.
                                self.bind(asname.as_str());
                            } else if !positive || exported(alias.name.as_str()).is_none() {
                                self.bind(alias.name.as_str());
                            }
                        }
                    }
                    _ => {}
                }
                visitor::walk_stmt(self, stmt);
            }

            fn visit_expr(&mut self, expr: &'a Expr) {
                // Explicit namespace reflection can escape through aliases and mutate any
                // imported name. Even a local/shadowed or qualified reference is treated
                // conservatively; resolving arbitrary runtime lookup is outside this rule.
                let name = match expr {
                    Expr::Name(name) => Some(name.id.as_str()),
                    Expr::Attribute(attribute) => Some(attribute.attr.as_str()),
                    _ => None,
                };
                if name.is_some_and(|name| {
                    matches!(
                        name,
                        "globals" | "locals" | "vars" | "exec" | "eval" | "__builtins__"
                    )
                }) {
                    self.reflective_namespace = true;
                }
                if let Expr::Named(walrus) = expr {
                    // Definition-time expressions can run outside the body scope that
                    // the AST visitor currently walks (defaults, decorators and bases).
                    // Treat every walrus target as uncertain, including local ones: a
                    // conservative recompute is safer than hiding a module rebinding.
                    let nested = self.nested;
                    self.nested = 0;
                    self.bind_target(&walrus.target);
                    self.nested = nested;
                }
                visitor::walk_expr(self, expr);
            }

            fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
                let ast::ExceptHandler::ExceptHandler(h) = handler;
                if let Some(name) = &h.name {
                    self.bind(name.as_str());
                }
                visitor::walk_except_handler(self, handler);
            }

            fn visit_pattern(&mut self, pattern: &'a ast::Pattern) {
                match pattern {
                    ast::Pattern::MatchAs(p) => {
                        if let Some(name) = &p.name {
                            self.bind(name.as_str());
                        }
                    }
                    ast::Pattern::MatchStar(p) => {
                        if let Some(name) = &p.name {
                            self.bind(name.as_str());
                        }
                    }
                    ast::Pattern::MatchMapping(p) => {
                        if let Some(rest) = &p.rest {
                            self.bind(rest.as_str());
                        }
                    }
                    _ => {}
                }
                visitor::walk_pattern(self, pattern);
            }
        }

        let mut bindings = Bindings {
            top_level_imports: &top_level_imports,
            module_aliases,
            mutated_exports: HashSet::new(),
            nested: 0,
            rebound: HashSet::new(),
            foreign_stars: Vec::new(),
            reflective_namespace: false,
        };
        visitor::walk_body(&mut bindings, body);
        bindings.rebound.extend(conflicting);

        let mut bound = bindings.rebound.clone();
        bound.extend(imported.keys().cloned());
        bound.extend(bindings.mutated_exports.iter().map(|name| name.to_string()));
        if !bindings.foreign_stars.is_empty() {
            bound.extend(exported_names().map(str::to_string));
        }
        let candidates: HashMap<String, &'static str> = imported
            .iter()
            .filter(|(name, (canonical, at))| {
                !bindings.mutated_exports.contains(canonical)
                    && !bindings.rebound.contains(*name)
                    && !bindings.foreign_stars.iter().any(|star| star > at)
            })
            .map(|(name, (canonical, _))| (name.clone(), *canonical))
            .collect();
        let proven = if bindings.reflective_namespace {
            HashMap::new()
        } else {
            candidates.clone()
        };
        Self {
            proven,
            candidates,
            bound,
            mutated_exports: bindings.mutated_exports,
        }
    }
}

/// Check every barca call in the decorators of one function: the decorators themselves and
/// the helpers inside their arguments (`partitions(...)`, `collect(...)`, `Schedule(...)`).
/// Returns the first problem, in source order.
///
/// Only names in `barca` ([`BarcaNames`]) are checked: any other `asset`, `task` or `collect`
/// is somebody else's function, and its arguments are its own business.
pub fn check_decorators(decorators: &[ast::Decorator], barca: &BarcaNames) -> Option<Problem> {
    /// The helper calls inside a decorator's arguments.
    struct Helpers<'f> {
        barca: &'f BarcaNames,
        found: Option<Problem>,
    }
    impl<'a> Visitor<'a> for Helpers<'_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if self.found.is_some() {
                return;
            }
            if let Expr::Call(call) = expr
                && let Some(name) = self.barca.resolve(&call.func)
                && let Some(sig) = Signature::named(name)
                && !sig.decorator
            {
                self.found = check_call(call, sig);
                if self.found.is_some() {
                    return;
                }
            }
            visitor::walk_expr(self, expr);
        }
    }

    for decorator in decorators {
        // Only a call to one of barca's decorators is looked at, and only its arguments.
        let Expr::Call(call) = &decorator.expression else {
            continue;
        };
        let Some(sig) = barca
            .resolve(&call.func)
            .and_then(Signature::named)
            .filter(|s| s.decorator)
        else {
            continue;
        };
        if let Some(problem) = check_call(call, sig) {
            return Some(problem);
        }
        let mut helpers = Helpers { barca, found: None };
        visitor::walk_arguments(&mut helpers, &call.arguments);
        if helpers.found.is_some() {
            return helpers.found;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruff_python_ast::Stmt;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn suggestions_are_offered_only_when_one_name_is_close() {
        let asset = Signature::named("asset").unwrap();
        assert_eq!(asset.closest("input"), Some("inputs"));
        assert_eq!(asset.closest("partition"), Some("partitions"));
        assert_eq!(asset.closest("serialiser"), Some("serializer"));
        assert_eq!(asset.closest("freshnes"), Some("freshness"));
        assert_eq!(asset.closest("tag"), Some("tags"));
        assert_eq!(asset.closest("evn"), Some("env")); // two neighbours swapped
        // Nothing close: no guess.
        assert_eq!(asset.closest("after"), None);
        assert_eq!(asset.closest("when"), None);
        assert_eq!(asset.closest("timeout"), None);
        // `retry` is as far from `retries` as from nothing else, but three edits: no guess.
        assert_eq!(asset.closest("retry"), None);
        // A short name gets one edit only: `tab` is two from `tags`.
        assert_eq!(asset.closest("tab"), None);
        // The helpers take no keywords, so there is nothing to suggest.
        assert_eq!(Signature::named("collect").unwrap().closest("asset"), None);
    }

    /// The flat form other tables are meant to be keyed to: one row per argument, with no
    /// duplicates, covering exactly what `SIGNATURES` lists.
    #[test]
    fn the_flat_list_has_every_argument_once() {
        let all: Vec<Argument> = arguments().collect();
        let expected: usize = SIGNATURES
            .iter()
            .map(|s| s.positional.len() + s.keywords.len())
            .sum();
        assert_eq!(all.len(), expected);
        let unique: std::collections::HashSet<(&str, &str)> =
            all.iter().map(|a| (a.call, a.name)).collect();
        assert_eq!(unique.len(), all.len());
        assert!(all.contains(&Argument {
            call: "sink",
            name: "path",
            passing: Passing::Positional
        }));
        assert!(all.contains(&Argument {
            call: "task",
            name: "partitions",
            passing: Passing::Keyword
        }));
        // What the parser reads on one node kind it reads on all three; `inputs` on a sensor
        // is the one difference, and it is an error of its own.
        let keywords = |call: &str| -> Vec<&str> {
            all.iter()
                .filter(|a| a.call == call)
                .map(|a| a.name)
                .collect()
        };
        assert_eq!(keywords("asset"), keywords("task"));
        let mut sensor = keywords("asset");
        sensor.retain(|k| *k != "inputs");
        assert_eq!(keywords("sensor"), sensor);
    }

    #[test]
    fn two_close_names_give_no_suggestion() {
        let sig = Signature {
            name: "x",
            decorator: true,
            positional: &[],
            keywords: &["inputs", "input_s"],
            deferred: &[],
            usage: "",
            topic: "",
        };
        assert_eq!(sig.closest("input"), None);
    }

    /// Parameters of a stub: (positional-only, accepted by keyword), `self` and the decorated
    /// function `fn` left out. Panics on `*args` / `**kwargs`: a stub that takes anything
    /// would run code that barca rejects.
    fn stub_parameters(name: &str, params: &ast::Parameters) -> (Vec<String>, Vec<String>) {
        assert!(
            params.vararg.is_none() && params.kwarg.is_none(),
            "the stub `{name}` takes *args or **kwargs: it would accept arguments barca rejects"
        );
        let own = |n: &str| n != "self" && n != "fn";
        let positional = params
            .posonlyargs
            .iter()
            .map(|p| p.name().to_string())
            .filter(|n| own(n))
            .collect();
        let keywords = params
            .args
            .iter()
            .chain(&params.kwonlyargs)
            .map(|p| p.name().to_string())
            .filter(|n| own(n))
            .collect();
        (positional, keywords)
    }

    /// The Python stubs are what a type checker, an IDE and `python pipeline.py` see. Their
    /// signatures must accept exactly what the check accepts.
    #[test]
    fn the_python_stubs_accept_exactly_these_arguments() {
        let path = repo_root().join("python/barca/__init__.py");
        let source = std::fs::read_to_string(&path).unwrap();
        let module = ruff_python_parser::parse_module(&source)
            .unwrap()
            .into_syntax();

        let mut seen = Vec::new();
        for stmt in &module.body {
            let (name, params) = match stmt {
                Stmt::FunctionDef(f) => (f.name.as_str(), &*f.parameters),
                Stmt::ClassDef(c) => {
                    let init = c.body.iter().find_map(|s| match s {
                        Stmt::FunctionDef(f) if f.name.as_str() == "__init__" => Some(f),
                        _ => None,
                    });
                    match init {
                        Some(f) => (c.name.as_str(), &*f.parameters),
                        None => continue,
                    }
                }
                _ => continue,
            };
            let Some(sig) = Signature::named(name) else {
                continue;
            };
            let (positional, keywords) = stub_parameters(name, params);
            assert_eq!(
                keywords, sig.keywords,
                "keyword arguments of `{name}` in python/barca/__init__.py differ from \
                 SIGNATURES in crates/barca-core/src/decorator_args.rs"
            );
            assert_eq!(
                positional, sig.positional,
                "positional-only parameters of `{name}` in python/barca/__init__.py differ \
                 from SIGNATURES"
            );
            seen.push(name.to_string());
        }
        let expected: Vec<&str> = SIGNATURES.iter().map(|s| s.name).collect();
        seen.sort();
        let mut sorted = expected.clone();
        sorted.sort();
        assert_eq!(
            seen, sorted,
            "every entry of SIGNATURES needs a stub, and one only"
        );
    }

    #[test]
    fn walrus_bindings_are_conservative_in_every_definition_scope() {
        for statement in [
            "def install(x=(asset := custom)): pass",
            "@(asset := custom)\ndef install(): pass",
            "class Install((asset := custom)): pass",
            "@(asset := custom)\nclass Install: pass",
            "def install():\n    (asset := custom)",
            "def install():\n    global asset\n    asset = custom",
            "def outer():\n    asset = custom\n    def inner():\n        nonlocal asset\n        (asset := custom)",
        ] {
            let source = format!("from barca import asset\n{statement}\n");
            let parsed = ruff_python_parser::parse_module(&source).unwrap();
            assert!(
                !BarcaNames::of(&parsed.syntax().body).contains("asset"),
                "{statement}"
            );
        }
    }

    #[test]
    fn explicit_namespace_reflection_makes_every_imported_name_uncertain() {
        for expression in ["globals", "locals", "vars", "exec", "eval", "__builtins__"] {
            for use_ in [
                format!("namespace = {expression}"),
                format!("from builtins import {expression} as namespace"),
                format!("namespace = builtins.{expression}"),
                format!("def install(default={expression}): pass"),
                format!("def install():\n    return {expression}"),
                format!("def install():\n    return foreign.{expression}"),
            ] {
                let source =
                    format!("from barca import asset, sensor, task, collect, unsafe\n{use_}\n");
                let parsed = ruff_python_parser::parse_module(&source).unwrap();
                let names = BarcaNames::of(&parsed.syntax().body);
                for name in ["asset", "sensor", "task", "collect", "unsafe"] {
                    assert!(!names.contains(name), "{use_}: {name}");
                }
            }
        }
    }

    /// The manual and the site list the accepted arguments in one table, generated from
    /// [`Signature::doc_row`].
    #[test]
    fn the_manual_and_the_site_list_exactly_these_arguments() {
        for file in [
            "crates/barca-cli/docs/assets.md",
            "site/src/content/docs/reference/api/decorators.md",
        ] {
            let text = std::fs::read_to_string(repo_root().join(file)).unwrap();
            let rows: Vec<&str> = text
                .lines()
                .skip_while(|l| !l.starts_with("| Call | Positional arguments |"))
                .skip(2)
                .take_while(|l| l.starts_with('|'))
                .collect();
            let expected: Vec<String> = SIGNATURES.iter().map(Signature::doc_row).collect();
            assert_eq!(
                rows,
                expected,
                "the accepted-arguments table in {file} differs from SIGNATURES. It should \
                 read:\n| Call | Positional arguments | Keyword arguments |\n|---|---|---|\n{}",
                expected.join("\n")
            );
        }
    }
}
