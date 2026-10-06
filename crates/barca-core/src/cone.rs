//! Dependency cone analysis — computes the transitive set of module-level
//! definitions that a function references.
//!
//! Pure function: (source, function_name) → cone hash string.
//! Uses ruff's AST for name resolution within a single file.
//!
//! Project modules (other `.py` files next to and below the pipeline file) are followed
//! through both import styles, at the same precision: `from helpers import compute` and
//! `import helpers` + `helpers.compute()` (also `import pkg.mod as m` + `m.f()`,
//! `import pkg.mod` + `pkg.mod.f()`, `from pkg import mod` + `mod.f()`) each add only
//! `compute`'s own cone — its source and the definitions it uses, transitively — never the
//! whole module. Modules that are not project files (stdlib, installed packages) add nothing.

use ruff_python_ast::{Expr, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// Compute the dependency cone hash for a function within a source file.
///
/// Walks the function body, finds all names that resolve to module-level
/// definitions (helpers, constants, imports), traces them transitively, and
/// hashes the combined source text.
///
/// `other_sources` maps module names to their source content (for cross-file resolution).
/// e.g., `{"helpers": "def compute(x): return x * 2\n"}`.
///
/// Returns empty string if the function has no module-level dependencies.
pub fn cone_hash(source: &str, function_name: &str) -> String {
    cone_hash_with_imports(source, function_name, &HashMap::new())
}

/// Cone hash with cross-file import resolution.
pub fn cone_hash_with_imports(
    source: &str,
    function_name: &str,
    other_sources: &HashMap<String, String>,
) -> String {
    let defs = collect_module_definitions(source);
    cone_hash_from_defs(&defs, function_name, other_sources, &HashSet::new())
}

/// Cone hash from pre-parsed module definitions. Avoids re-parsing the source
/// when computing hashes for multiple functions in the same file.
///
/// `packages` is the set of dotted module names in `other_sources` that are
/// `__init__.py` packages (as opposed to regular submodules) — needed to
/// resolve relative imports correctly (see `resolve_import`).
pub fn cone_hash_from_defs(
    defs: &HashMap<String, ModuleDef>,
    function_name: &str,
    other_sources: &HashMap<String, String>,
    packages: &HashSet<String>,
) -> String {
    let Some(target) = defs.get(function_name) else {
        return String::new();
    };

    let refs = match target {
        ModuleDef::Function { references, .. } => references,
        _ => return String::new(),
    };

    // BFS through transitive dependencies.
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = refs
        .iter()
        .filter(|r| traceable(defs, r))
        .cloned()
        .collect();

    let mut cone_parts: Vec<(String, String)> = Vec::new();
    trace_module_attributes(
        refs,
        defs,
        other_sources,
        packages,
        &mut visited,
        &mut cone_parts,
        0,
    );

    while let Some(name) = queue.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(def) = defs.get(&name) else {
            continue;
        };
        match def {
            ModuleDef::Function {
                source_text,
                references,
            }
            | ModuleDef::Assignment {
                source_text,
                references,
            } => {
                cone_parts.push((name.clone(), source_text.clone()));
                for r in references {
                    if !visited.contains(r) && traceable(defs, r) {
                        queue.push(r.clone());
                    }
                }
                trace_module_attributes(
                    references,
                    defs,
                    other_sources,
                    packages,
                    &mut visited,
                    &mut cone_parts,
                    0,
                );
            }
            // Only reached through `module.attr`, handled by `trace_module_attributes`.
            ModuleDef::ModuleImport { .. } => {}
            ModuleDef::Import { module } => {
                // Resolve cross-file import, following re-export chains.
                resolve_import(
                    module,
                    &name,
                    other_sources,
                    packages,
                    &mut visited,
                    &mut cone_parts,
                    0, // depth limit to prevent infinite loops
                );
            }
        }
    }

    if cone_parts.is_empty() {
        return String::new();
    }

    // Sort by name for determinism, then hash.
    cone_parts.sort_by_key(|(name, _)| name.clone());
    let mut hasher = Sha256::new();
    for (name, text) in &cone_parts {
        hasher.update(name.as_bytes());
        hasher.update(b":");
        hasher.update(text.as_bytes());
        hasher.update(b"\n");
    }
    format!("{:x}", hasher.finalize())
}

// ─── Internal types ──────────────────────────────────────────────────────────

pub enum ModuleDef {
    Function {
        source_text: String,
        references: HashSet<String>,
    },
    Assignment {
        source_text: String,
        references: HashSet<String>,
    },
    /// `from module import name`: the name is bound to `module`'s definition of it.
    Import { module: String },
    /// `import module` / `import a.b as m`: the name is bound to the module itself
    /// (`import a.b` binds `a`). Followed only through attribute access (`m.f`).
    ModuleImport { module: String },
}

/// A name the BFS follows by itself. A bare module binding is not: what a step uses from a
/// module is the attribute it reads (`helpers.compute`), traced by `trace_module_attributes`.
fn traceable(defs: &HashMap<String, ModuleDef>, name: &str) -> bool {
    matches!(defs.get(name), Some(d) if !matches!(d, ModuleDef::ModuleImport { .. }))
}

/// The project module and attribute a dotted reference (`helpers.compute`, `m.f`,
/// `pkg.mod.f`) reads, when its first segment is bound to a project module in `defs`.
/// `None` for anything else — attribute access on a local value, or on a module that is not a
/// project file (`json.dumps`), which therefore leaves the hash exactly as before.
fn module_attribute(
    reference: &str,
    defs: &HashMap<String, ModuleDef>,
    other_sources: &HashMap<String, String>,
) -> Option<(String, String)> {
    let mut parts = reference.split('.');
    let base = parts.next()?;
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        return None;
    }
    let module = match defs.get(base)? {
        ModuleDef::ModuleImport { module } => module.clone(),
        // `from pkg import mod` where `pkg.mod` is itself a project module.
        ModuleDef::Import { module } => {
            let full = if module.is_empty() {
                base.to_string()
            } else {
                format!("{module}.{base}")
            };
            if !other_sources.contains_key(&full) {
                return None;
            }
            full
        }
        _ => return None,
    };
    // The longest prefix of the chain that is a project module; the next segment is the
    // attribute read from it (`pkg.mod.f` → module `pkg.mod`, attribute `f`).
    for k in (0..rest.len()).rev() {
        let candidate = if k == 0 {
            module.clone()
        } else {
            format!("{module}.{}", rest[..k].join("."))
        };
        if other_sources.contains_key(&candidate) {
            let attr = rest[k];
            if other_sources.contains_key(&format!("{candidate}.{attr}")) {
                // `pkg.mod` names a submodule; the longer chain (`pkg.mod.f`) says what is used.
                return None;
            }
            return Some((candidate, attr.to_string()));
        }
    }
    None
}

/// Add the cone of every project-module attribute in `references` (`helpers.compute`), exactly
/// as `from helpers import compute` would.
fn trace_module_attributes(
    references: &HashSet<String>,
    defs: &HashMap<String, ModuleDef>,
    other_sources: &HashMap<String, String>,
    packages: &HashSet<String>,
    visited: &mut HashSet<String>,
    cone_parts: &mut Vec<(String, String)>,
    depth: usize,
) {
    let mut attrs: Vec<(String, String)> = references
        .iter()
        .filter(|r| r.contains('.'))
        .filter_map(|r| module_attribute(r, defs, other_sources))
        .collect();
    attrs.sort();
    attrs.dedup();
    for (module, attr) in attrs {
        // Same key `resolve_import` uses for a module's definitions, so a definition reached
        // both ways is hashed once.
        if visited.insert(format!("{module}:{attr}")) {
            resolve_import(
                &module,
                &attr,
                other_sources,
                packages,
                visited,
                cone_parts,
                depth,
            );
        }
    }
}

/// Resolve a cross-file import, following re-export chains up to a depth limit.
fn resolve_import(
    module: &str,
    name: &str,
    other_sources: &HashMap<String, String>,
    packages: &HashSet<String>,
    visited: &mut HashSet<String>,
    cone_parts: &mut Vec<(String, String)>,
    depth: usize,
) {
    if depth > 5 {
        cone_parts.push((name.to_string(), format!("import:{module}:{name}")));
        return;
    }

    let Some(module_source) = other_sources.get(module) else {
        cone_parts.push((name.to_string(), format!("import:{module}:{name}")));
        return;
    };

    // The package a relative import inside `module` resolves against: for an
    // `__init__.py` package, that's the module's own dotted name; for a
    // regular submodule (`pkg.sub`), it's one level up (`pkg`).
    let own_package = if packages.contains(module) {
        Some(module.to_string())
    } else {
        module.rsplit_once('.').map(|(pkg, _)| pkg.to_string())
    };

    let imported_defs =
        collect_module_definitions_with_context(module_source, own_package.as_deref());
    // A module binding (`import x` inside `module`) is not a definition of `name`; it hashes
    // as the unresolved placeholder, exactly as before module bindings were recorded.
    let Some(imported_def) = imported_defs
        .get(name)
        .filter(|d| !matches!(d, ModuleDef::ModuleImport { .. }))
    else {
        cone_parts.push((name.to_string(), format!("import:{module}:{name}")));
        return;
    };

    match imported_def {
        ModuleDef::Function {
            source_text,
            references,
        }
        | ModuleDef::Assignment {
            source_text,
            references,
        } => {
            cone_parts.push((format!("{module}:{name}"), source_text.clone()));
            trace_module_attributes(
                references,
                &imported_defs,
                other_sources,
                packages,
                visited,
                cone_parts,
                depth + 1,
            );
            // Trace transitive deps within the module.
            let mut bfs_queue: Vec<String> = references
                .iter()
                .filter(|r| traceable(&imported_defs, r))
                .cloned()
                .collect();
            while let Some(dep) = bfs_queue.pop() {
                let dep_key = format!("{module}:{dep}");
                if !visited.insert(dep_key.clone()) {
                    continue;
                }
                match imported_defs.get(&dep) {
                    Some(
                        ModuleDef::Function {
                            source_text,
                            references,
                        }
                        | ModuleDef::Assignment {
                            source_text,
                            references,
                        },
                    ) => {
                        cone_parts.push((dep_key, source_text.clone()));
                        for r in references {
                            if !visited.contains(&format!("{module}:{r}"))
                                && traceable(&imported_defs, r)
                            {
                                bfs_queue.push(r.clone());
                            }
                        }
                        trace_module_attributes(
                            references,
                            &imported_defs,
                            other_sources,
                            packages,
                            visited,
                            cone_parts,
                            depth + 1,
                        );
                    }
                    Some(ModuleDef::Import {
                        module: next_module,
                    }) => {
                        // `dep` is itself imported from a third file — resolve
                        // it there instead of silently dropping it, or a
                        // change to its actual definition would never
                        // invalidate the cache.
                        resolve_import(
                            next_module,
                            &dep,
                            other_sources,
                            packages,
                            visited,
                            cone_parts,
                            depth + 1,
                        );
                    }
                    Some(ModuleDef::ModuleImport { .. }) | None => {}
                }
            }
        }
        // Filtered out above.
        ModuleDef::ModuleImport { .. } => {}
        ModuleDef::Import {
            module: next_module,
        } => {
            // Re-export chain: follow to the next module recursively.
            resolve_import(
                next_module,
                name,
                other_sources,
                packages,
                visited,
                cone_parts,
                depth + 1,
            );
        }
    }
}

// ─── Module definition collection ────────────────────────────────────────────

pub fn collect_module_definitions(source: &str) -> HashMap<String, ModuleDef> {
    collect_module_definitions_with_context(source, None)
}

/// Walks `levels_up` package levels above `package` (e.g. `levels_up=1` on
/// `"pkg.sub"` yields `"pkg"`). Returns `None` if there aren't enough levels
/// (e.g. `from ... import x` inside a module that isn't nested that deep).
fn package_ancestor(package: &str, levels_up: usize) -> Option<String> {
    let mut current = package;
    for _ in 0..levels_up {
        current = current.rsplit_once('.')?.0;
    }
    Some(current.to_string())
}

fn collect_module_definitions_with_context(
    source: &str,
    // The package this module resolves relative imports against: for an
    // `__init__.py`, its own dotted name; for a regular submodule, its
    // parent package. `None` for the entry file (no cross-file context).
    own_package: Option<&str>,
) -> HashMap<String, ModuleDef> {
    let Ok(parsed) = parse_module(source) else {
        return HashMap::new();
    };

    let module = parsed.into_syntax();
    let mut defs: HashMap<String, ModuleDef> = HashMap::new();

    for stmt in &module.body {
        match stmt {
            Stmt::FunctionDef(func) => {
                let name = func.name.to_string();
                let start = func.range().start().to_usize();
                let end = func.range().end().to_usize();
                let source_text = source[start..end].to_string();
                let references = collect_names_from_stmts(&func.body);
                defs.insert(
                    name,
                    ModuleDef::Function {
                        source_text,
                        references,
                    },
                );
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Expr::Name(n) = target {
                        let name = n.id.to_string();
                        let start = assign.range().start().to_usize();
                        let end = assign.range().end().to_usize();
                        let source_text = source[start..end].to_string();
                        let references = collect_names_from_expr(&assign.value);
                        defs.insert(
                            name,
                            ModuleDef::Assignment {
                                source_text,
                                references,
                            },
                        );
                    }
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Expr::Name(n) = assign.target.as_ref() {
                    let name = n.id.to_string();
                    let start = assign.range().start().to_usize();
                    let end = assign.range().end().to_usize();
                    let source_text = source[start..end].to_string();
                    let references = assign
                        .value
                        .as_ref()
                        .map(|v| collect_names_from_expr(v))
                        .unwrap_or_default();
                    defs.insert(
                        name,
                        ModuleDef::Assignment {
                            source_text,
                            references,
                        },
                    );
                }
            }
            Stmt::Import(import) => {
                // `import a.b` binds `a`; `import a.b as m` binds `m` to `a.b`. A name that is
                // already defined keeps its earlier definition (`or_insert`), so recording module
                // bindings never changes what an existing name resolves to.
                for alias in &import.names {
                    let full = alias.name.to_string();
                    let (bound, module) = match &alias.asname {
                        Some(asname) => (asname.to_string(), full),
                        None => {
                            let top = full.split('.').next().unwrap_or_default().to_string();
                            (top.clone(), top)
                        }
                    };
                    defs.entry(bound)
                        .or_insert(ModuleDef::ModuleImport { module });
                }
            }
            Stmt::ImportFrom(import) => {
                let raw_module = import
                    .module
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_default();
                // Resolve relative imports: `from .core import x` (level=1)
                // resolves against `own_package`; each extra leading dot
                // (level=2, 3, ...) walks one more package level up.
                let module_name = if import.level > 0 {
                    match own_package.and_then(|pkg| {
                        package_ancestor(pkg, (import.level as usize).saturating_sub(1))
                    }) {
                        Some(base) if raw_module.is_empty() => base,
                        Some(base) => format!("{base}.{raw_module}"),
                        None => raw_module,
                    }
                } else {
                    raw_module
                };
                for alias in &import.names {
                    let imported_name = alias
                        .asname
                        .as_ref()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| alias.name.to_string());
                    defs.insert(
                        imported_name,
                        ModuleDef::Import {
                            module: module_name.clone(),
                        },
                    );
                }
            }
            _ => {}
        }
    }

    defs
}

// ─── Name collection from AST ────────────────────────────────────────────────

fn collect_names_from_stmts(stmts: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    for stmt in stmts {
        collect_stmt_names(stmt, &mut names);
    }
    names
}

fn collect_names_from_expr(expr: &Expr) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_expr_names(expr, &mut names);
    names
}

fn collect_stmt_names(stmt: &Stmt, names: &mut HashSet<String>) {
    match stmt {
        Stmt::Expr(s) => collect_expr_names(&s.value, names),
        Stmt::Return(s) => {
            if let Some(v) = &s.value {
                collect_expr_names(v, names);
            }
        }
        Stmt::Assign(s) => {
            collect_expr_names(&s.value, names);
        }
        Stmt::AugAssign(s) => {
            collect_expr_names(&s.target, names);
            collect_expr_names(&s.value, names);
        }
        Stmt::AnnAssign(s) => {
            if let Some(v) = &s.value {
                collect_expr_names(v, names);
            }
        }
        Stmt::If(s) => {
            collect_expr_names(&s.test, names);
            for st in &s.body {
                collect_stmt_names(st, names);
            }
            for clause in &s.elif_else_clauses {
                if let Some(test) = &clause.test {
                    collect_expr_names(test, names);
                }
                for st in &clause.body {
                    collect_stmt_names(st, names);
                }
            }
        }
        Stmt::For(s) => {
            collect_expr_names(&s.iter, names);
            for st in &s.body {
                collect_stmt_names(st, names);
            }
        }
        Stmt::While(s) => {
            collect_expr_names(&s.test, names);
            for st in &s.body {
                collect_stmt_names(st, names);
            }
        }
        Stmt::With(s) => {
            for item in &s.items {
                collect_expr_names(&item.context_expr, names);
            }
            for st in &s.body {
                collect_stmt_names(st, names);
            }
        }
        Stmt::FunctionDef(s) => {
            for st in &s.body {
                collect_stmt_names(st, names);
            }
        }
        Stmt::Try(s) => {
            for st in &s.body {
                collect_stmt_names(st, names);
            }
            for handler in &s.handlers {
                if let Some(h) = handler.as_except_handler() {
                    for st in &h.body {
                        collect_stmt_names(st, names);
                    }
                }
            }
            for st in &s.orelse {
                collect_stmt_names(st, names);
            }
            for st in &s.finalbody {
                collect_stmt_names(st, names);
            }
        }
        Stmt::Match(s) => {
            collect_expr_names(&s.subject, names);
            for case in &s.cases {
                if let Some(guard) = &case.guard {
                    collect_expr_names(guard, names);
                }
                for st in &case.body {
                    collect_stmt_names(st, names);
                }
            }
        }
        Stmt::ClassDef(s) => {
            for arg in s.arguments.iter().flat_map(|a| a.args.iter()) {
                collect_expr_names(arg, names);
            }
            for st in &s.body {
                collect_stmt_names(st, names);
            }
        }
        Stmt::Raise(s) => {
            if let Some(exc) = &s.exc {
                collect_expr_names(exc, names);
            }
            if let Some(cause) = &s.cause {
                collect_expr_names(cause, names);
            }
        }
        Stmt::Assert(s) => {
            collect_expr_names(&s.test, names);
            if let Some(msg) = &s.msg {
                collect_expr_names(msg, names);
            }
        }
        Stmt::Delete(s) => {
            for target in &s.targets {
                collect_expr_names(target, names);
            }
        }
        _ => {}
    }
}

/// `a.b.c` for an attribute chain rooted at a plain name; `None` otherwise (`f().x`).
fn dotted_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(format!("{}.{}", dotted_name(&a.value)?, a.attr)),
        _ => None,
    }
}

fn collect_expr_names(expr: &Expr, names: &mut HashSet<String>) {
    match expr {
        Expr::Name(n) => {
            names.insert(n.id.to_string());
        }
        Expr::Call(c) => {
            collect_expr_names(&c.func, names);
            for arg in c.arguments.args.iter() {
                collect_expr_names(arg, names);
            }
            for kw in c.arguments.keywords.iter() {
                collect_expr_names(&kw.value, names);
            }
        }
        Expr::Attribute(a) => {
            // `helpers.compute` also records the dotted chain, so a call through an imported
            // project module can be traced (`trace_module_attributes`). The base name is still
            // recorded (via the recursion), as before.
            if let Some(chain) = dotted_name(expr) {
                names.insert(chain);
            }
            collect_expr_names(&a.value, names)
        }
        Expr::Subscript(s) => {
            collect_expr_names(&s.value, names);
            collect_expr_names(&s.slice, names);
        }
        Expr::BinOp(b) => {
            collect_expr_names(&b.left, names);
            collect_expr_names(&b.right, names);
        }
        Expr::UnaryOp(u) => collect_expr_names(&u.operand, names),
        Expr::BoolOp(b) => {
            for v in &b.values {
                collect_expr_names(v, names);
            }
        }
        Expr::Compare(c) => {
            collect_expr_names(&c.left, names);
            for v in &c.comparators {
                collect_expr_names(v, names);
            }
        }
        Expr::If(i) => {
            collect_expr_names(&i.test, names);
            collect_expr_names(&i.body, names);
            collect_expr_names(&i.orelse, names);
        }
        Expr::Dict(d) => {
            for item in &d.items {
                if let Some(k) = &item.key {
                    collect_expr_names(k, names);
                }
                collect_expr_names(&item.value, names);
            }
        }
        Expr::List(l) => {
            for e in &l.elts {
                collect_expr_names(e, names);
            }
        }
        Expr::Tuple(t) => {
            for e in &t.elts {
                collect_expr_names(e, names);
            }
        }
        Expr::Lambda(l) => collect_expr_names(&l.body, names),
        Expr::Starred(s) => collect_expr_names(&s.value, names),
        Expr::ListComp(c) => {
            collect_expr_names(&c.elt, names);
            for comp in &c.generators {
                collect_expr_names(&comp.iter, names);
                for cond in &comp.ifs {
                    collect_expr_names(cond, names);
                }
            }
        }
        Expr::SetComp(c) => {
            collect_expr_names(&c.elt, names);
            for comp in &c.generators {
                collect_expr_names(&comp.iter, names);
                for cond in &comp.ifs {
                    collect_expr_names(cond, names);
                }
            }
        }
        Expr::DictComp(c) => {
            if let Some(k) = &c.key {
                collect_expr_names(k, names);
            }
            collect_expr_names(&c.value, names);
            for comp in &c.generators {
                collect_expr_names(&comp.iter, names);
                for cond in &comp.ifs {
                    collect_expr_names(cond, names);
                }
            }
        }
        Expr::Generator(g) => {
            collect_expr_names(&g.elt, names);
            for comp in &g.generators {
                collect_expr_names(&comp.iter, names);
                for cond in &comp.ifs {
                    collect_expr_names(cond, names);
                }
            }
        }
        Expr::FString(f) => {
            for part in &f.value {
                if let ruff_python_ast::FStringPart::FString(fstr) = part {
                    for interp in fstr.elements.interpolations() {
                        collect_expr_names(&interp.expression, names);
                    }
                }
            }
        }
        Expr::Named(n) => {
            collect_expr_names(&n.value, names);
        }
        Expr::Await(a) => collect_expr_names(&a.value, names),
        Expr::Yield(y) => {
            if let Some(v) = &y.value {
                collect_expr_names(v, names);
            }
        }
        Expr::YieldFrom(y) => collect_expr_names(&y.value, names),
        Expr::Slice(s) => {
            if let Some(l) = &s.lower {
                collect_expr_names(l, names);
            }
            if let Some(u) = &s.upper {
                collect_expr_names(u, names);
            }
            if let Some(st) = &s.step {
                collect_expr_names(st, names);
            }
        }
        _ => {}
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_deps() {
        let src = r#"
from barca import asset

@asset()
def simple():
    return {"value": 1}
"#;
        let h = cone_hash(src, "simple");
        assert!(h.is_empty()); // no module-level deps
    }

    #[test]
    fn test_helper_function() {
        let src = r#"
def compute(x):
    return x * 2

def my_asset():
    return compute(21)
"#;
        let h = cone_hash(src, "my_asset");
        assert!(!h.is_empty()); // depends on compute
    }

    #[test]
    fn test_global_constant() {
        let src = r#"
THRESHOLD = 0.5

def check():
    return THRESHOLD > 0.3
"#;
        let h = cone_hash(src, "check");
        assert!(!h.is_empty()); // depends on THRESHOLD
    }

    #[test]
    fn test_transitive_deps() {
        let src = r#"
def step_a():
    return 1

def step_b():
    return step_a() + 1

def result():
    return step_b() + 1
"#;
        let h = cone_hash(src, "result");
        assert!(!h.is_empty());
        // Changing step_a should change the cone hash
        let src2 = src.replace("return 1", "return 99");
        let h2 = cone_hash(&src2, "result");
        assert_ne!(h, h2);
    }

    #[test]
    fn test_helper_change_changes_hash() {
        let src1 = r#"
def helper():
    return 1

def my_asset():
    return helper()
"#;
        let src2 = r#"
def helper():
    return 999

def my_asset():
    return helper()
"#;
        let h1 = cone_hash(src1, "my_asset");
        let h2 = cone_hash(src2, "my_asset");
        assert_ne!(h1, h2); // helper changed → cone hash changed
    }

    #[test]
    fn test_constant_change_changes_hash() {
        let src1 = r#"
THRESHOLD = 0.5

def check():
    return THRESHOLD > 0.3
"#;
        let src2 = r#"
THRESHOLD = 100

def check():
    return THRESHOLD > 0.3
"#;
        let h1 = cone_hash(src1, "check");
        let h2 = cone_hash(src2, "check");
        assert_ne!(h1, h2); // constant changed → cone hash changed
    }

    #[test]
    fn test_unrelated_change_no_effect() {
        let src1 = r#"
def unrelated():
    return "not used"

def my_asset():
    return {"value": 1}
"#;
        let src2 = r#"
def unrelated():
    return "changed but irrelevant"

def my_asset():
    return {"value": 1}
"#;
        let h1 = cone_hash(src1, "my_asset");
        let h2 = cone_hash(src2, "my_asset");
        assert_eq!(h1, h2); // unrelated function changed → no effect
    }

    #[test]
    fn test_deterministic() {
        let src = r#"
def helper():
    return 42

def my_asset():
    return helper()
"#;
        let h1 = cone_hash(src, "my_asset");
        let h2 = cone_hash(src, "my_asset");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_package_ancestor() {
        assert_eq!(package_ancestor("mylib", 0), Some("mylib".to_string()));
        assert_eq!(
            package_ancestor("mylib.core", 0),
            Some("mylib.core".to_string())
        );
        assert_eq!(package_ancestor("mylib.core", 1), Some("mylib".to_string()));
        assert_eq!(
            package_ancestor("mylib.sub.core", 1),
            Some("mylib.sub".to_string())
        );
        assert_eq!(
            package_ancestor("mylib.sub.core", 2),
            Some("mylib".to_string())
        );
        // Not enough segments to walk that many levels up.
        assert_eq!(package_ancestor("mylib", 1), None);
    }

    #[test]
    fn test_relative_import_in_regular_submodule_resolves_to_parent_package() {
        // mylib/core.py: `from .util import h` re-exports `h`. `core.py` is a
        // regular submodule (not in `packages`), so its relative import must
        // resolve against its *parent* package `mylib`, giving `mylib.util` —
        // not `mylib.core.util` (the pre-fix bug, using core's own name).
        let entry_src = "from mylib.core import h\n\ndef my_asset():\n    return h()\n";
        let core_src = "from .util import h\n";
        let util_src_v1 = "def h():\n    return 1\n";
        let util_src_v2 = "def h():\n    return 999\n";

        let mut other_sources = HashMap::new();
        other_sources.insert("mylib.core".to_string(), core_src.to_string());
        other_sources.insert("mylib.util".to_string(), util_src_v1.to_string());

        let mut packages = HashSet::new();
        packages.insert("mylib".to_string());

        let defs = collect_module_definitions(entry_src);
        let h1 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        // If `.util` had wrongly resolved to `mylib.core.util`, this lookup
        // would miss `other_sources` entirely and the cone hash would stay
        // constant regardless of `h`'s actual source.
        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        assert_ne!(
            h1, h2,
            "relative import in a regular submodule must resolve against its parent package"
        );
    }

    #[test]
    fn test_relative_import_level_two_walks_two_package_levels_up() {
        // mylib/sub/core.py: `from ..util import h` (level=2) must walk two
        // package levels up from its own package `mylib.sub` to `mylib`.
        let entry_src = "from mylib.sub.core import h\n\ndef my_asset():\n    return h()\n";
        let core_src = "from ..util import h\n";
        let util_src_v1 = "def h():\n    return 1\n";
        let util_src_v2 = "def h():\n    return 999\n";

        let mut other_sources = HashMap::new();
        other_sources.insert("mylib.sub.core".to_string(), core_src.to_string());
        other_sources.insert("mylib.util".to_string(), util_src_v1.to_string());

        let mut packages = HashSet::new();
        packages.insert("mylib".to_string());
        packages.insert("mylib.sub".to_string());

        let defs = collect_module_definitions(entry_src);
        let h1 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        assert_ne!(
            h1, h2,
            "level=2 relative import must walk two package levels up from the submodule's own package"
        );
    }

    #[test]
    fn test_relative_import_in_init_still_resolves_against_own_name() {
        // Regression: `__init__.py`'s own dotted name IS the package, so
        // `from .core import x` inside `mylib/__init__.py` must still
        // resolve to `mylib.core` (own_package == module's own name here).
        let entry_src = "from mylib import transform\n\ndef my_asset():\n    return transform(1)\n";
        let init_src = "from .core import transform\n";
        let core_src_v1 = "def transform(x):\n    return x * 2\n";
        let core_src_v2 = "def transform(x):\n    return x * 99\n";

        let mut other_sources = HashMap::new();
        other_sources.insert("mylib".to_string(), init_src.to_string());
        other_sources.insert("mylib.core".to_string(), core_src_v1.to_string());

        let mut packages = HashSet::new();
        packages.insert("mylib".to_string());

        let defs = collect_module_definitions(entry_src);
        let h1 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        other_sources.insert("mylib.core".to_string(), core_src_v2.to_string());
        let h2 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &packages);

        assert_ne!(h1, h2);
    }

    #[test]
    fn test_transitive_dep_that_is_itself_a_cross_file_import_is_tracked() {
        // mylib.core:helper() calls h(), which mylib.core imports from a
        // *third* file, mylib.util. The inner BFS that traces helper's
        // transitive deps used to only follow Function/Assignment defs and
        // silently drop `h` (an Import def), so changes to `h`'s real
        // definition in util.py were never detected — a silent stale-cache
        // bug, not specific to relative imports (plain absolute imports
        // reproduce it identically).
        let entry_src = "from mylib.core import helper\n\ndef my_asset():\n    return helper()\n";
        let core_src = "from mylib.util import h\n\ndef helper():\n    return h()\n";
        let util_src_v1 = "def h():\n    return 1\n";
        let util_src_v2 = "def h():\n    return 999\n";

        let mut other_sources = HashMap::new();
        other_sources.insert("mylib.core".to_string(), core_src.to_string());
        other_sources.insert("mylib.util".to_string(), util_src_v1.to_string());

        let defs = collect_module_definitions(entry_src);
        let h1 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &HashSet::new());

        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = cone_hash_from_defs(&defs, "my_asset", &other_sources, &HashSet::new());

        assert_ne!(
            h1, h2,
            "a helper's transitive dependency that is itself a cross-file import must be tracked"
        );
    }

    // ─── `import module` + `module.attr` (#178) ──────────────────────────────

    fn sources(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn hash_of(entry: &str, other: &HashMap<String, String>, packages: &[&str]) -> String {
        let packages: HashSet<String> = packages.iter().map(|p| p.to_string()).collect();
        cone_hash_from_defs(
            &collect_module_definitions(entry),
            "my_asset",
            other,
            &packages,
        )
    }

    const HELPERS_V1: &str = "def compute():\n    return 1\n\n\ndef unrelated():\n    return 0\n";
    const HELPERS_V2: &str = "def compute():\n    return 22\n\n\ndef unrelated():\n    return 0\n";
    const HELPERS_UNRELATED_EDIT: &str =
        "def compute():\n    return 1\n\n\ndef unrelated():\n    return 999\n";

    #[test]
    fn module_attribute_call_is_tracked() {
        let entry = "import helpers\n\ndef my_asset():\n    return helpers.compute()\n";
        let h1 = hash_of(entry, &sources(&[("helpers", HELPERS_V1)]), &[]);
        let h2 = hash_of(entry, &sources(&[("helpers", HELPERS_V2)]), &[]);
        assert!(!h1.is_empty());
        assert_ne!(h1, h2, "editing helpers.compute must change the cone hash");
    }

    #[test]
    fn module_attribute_call_hashes_only_the_used_functions_cone_like_from_import() {
        let attr = "import helpers\n\ndef my_asset():\n    return helpers.compute()\n";
        let from = "from helpers import compute\n\ndef my_asset():\n    return compute()\n";
        let v1 = sources(&[("helpers", HELPERS_V1)]);
        let edited = sources(&[("helpers", HELPERS_UNRELATED_EDIT)]);
        assert_eq!(
            hash_of(attr, &v1, &[]),
            hash_of(attr, &edited, &[]),
            "editing a function the step never uses must not change its hash"
        );
        assert_eq!(
            hash_of(attr, &v1, &[]),
            hash_of(from, &v1, &[]),
            "both import styles hash exactly the same cone"
        );
    }

    #[test]
    fn aliased_dotted_module_attribute_call_is_tracked() {
        let entry = "import pkg.mod as m\n\ndef my_asset():\n    return m.f()\n";
        let v1 = sources(&[("pkg", ""), ("pkg.mod", "def f():\n    return 1\n")]);
        let v2 = sources(&[("pkg", ""), ("pkg.mod", "def f():\n    return 22\n")]);
        let h1 = hash_of(entry, &v1, &["pkg"]);
        assert!(!h1.is_empty());
        assert_ne!(h1, hash_of(entry, &v2, &["pkg"]));
        let from = "from pkg.mod import f\n\ndef my_asset():\n    return f()\n";
        assert_eq!(h1, hash_of(from, &v1, &["pkg"]));
    }

    #[test]
    fn full_dotted_module_attribute_call_is_tracked() {
        // `import pkg.mod` binds `pkg`; the call goes through `pkg.mod.f`. Works whether or not
        // `pkg` has an `__init__.py` (namespace package).
        let entry = "import pkg.mod\n\ndef my_asset():\n    return pkg.mod.f()\n";
        for with_init in [true, false] {
            let mut v1 = sources(&[("pkg.mod", "def f():\n    return 1\n")]);
            let mut v2 = sources(&[("pkg.mod", "def f():\n    return 22\n")]);
            if with_init {
                v1.insert("pkg".into(), String::new());
                v2.insert("pkg".into(), String::new());
            }
            assert_ne!(hash_of(entry, &v1, &["pkg"]), hash_of(entry, &v2, &["pkg"]));
        }
    }

    #[test]
    fn from_package_import_module_then_attribute_call_is_tracked() {
        let entry = "from pkg import mod\n\ndef my_asset():\n    return mod.f()\n";
        let v1 = sources(&[("pkg", ""), ("pkg.mod", "def f():\n    return 1\n")]);
        let v2 = sources(&[("pkg", ""), ("pkg.mod", "def f():\n    return 22\n")]);
        assert_ne!(hash_of(entry, &v1, &["pkg"]), hash_of(entry, &v2, &["pkg"]));
    }

    #[test]
    fn module_attribute_constant_and_transitive_deps_are_tracked() {
        // A module-level constant read through the module, and the helper's own dependencies
        // (a local helper and another project module called by attribute) are all in the cone.
        let entry = "import helpers\n\nSCALE = helpers.BASE\n\ndef my_asset():\n    return helpers.compute() * SCALE\n";
        let helpers = |base: &str, inner: &str| {
            format!(
                "import other\n\nBASE = {base}\n\n\ndef _inner():\n    return {inner}\n\n\ndef compute():\n    return _inner() + other.g()\n"
            )
        };
        let other_v1 = "def g():\n    return 1\n";
        let other_v2 = "def g():\n    return 22\n";
        let base = hash_of(
            entry,
            &sources(&[("helpers", &helpers("2", "1")), ("other", other_v1)]),
            &[],
        );
        for (h, o) in [
            (helpers("3", "1"), other_v1),
            (helpers("2", "5"), other_v1),
            (helpers("2", "1"), other_v2),
        ] {
            assert_ne!(
                base,
                hash_of(entry, &sources(&[("helpers", &h), ("other", o)]), &[])
            );
        }
    }

    #[test]
    fn non_project_module_attribute_calls_leave_the_hash_unchanged() {
        // `json` is not a project module: the cone stays empty, exactly as before #178.
        let entry = "import json\nimport os.path as osp\n\ndef my_asset():\n    return json.dumps(osp.join('a', 'b'))\n";
        assert_eq!(
            hash_of(entry, &sources(&[("helpers", HELPERS_V1)]), &[]),
            ""
        );
    }

    /// Cone hashes from barca 0.10.0 for import styles that already worked. #178 must not move
    /// them, or every existing cache is invalidated.
    #[test]
    fn cone_hash_unchanged_for_from_imports_and_stdlib_modules() {
        let other = sources(&[
            (
                "helpers",
                "import json\n\ndef compute():\n    return json.dumps(1)\n",
            ),
            ("pkg", "from .core import t\n"),
            ("pkg.core", "def t(x):\n    return x\n"),
        ]);
        let entry = "import json\nfrom helpers import compute\nfrom pkg import t\nfrom numpy import array\n\nRATE = 2\n\ndef my_asset():\n    return [compute(), t(RATE), array([1]), json.dumps(2)]\n";
        assert_eq!(
            hash_of(entry, &other, &["pkg"]),
            "5f177ae7cd8027eb78d02d6cd8a7ec9c636aa91ae78576eabceac2386cc41382"
        );
    }
}
