//! What the top-level names of one Python file are bound to. A leaf of the frontend: the parser
//! ([`crate::parse`]) and group declarations ([`crate::groups`]) resolve references with it.

use ruff_python_ast::{Expr, Stmt};
use std::collections::HashMap;

use barca_model::model::NodeRef;

/// What the top-level names of a file are bound to, for resolving `inputs=` references:
/// functions defined in the file, names imported with `from M import x [as y]`, and modules
/// imported with `import M [as m]`. Module names keep their leading dots (`.sources`).
#[derive(Default)]
pub(crate) struct FileNames {
    local: std::collections::HashSet<String>,
    /// local name -> (module, name in that module)
    pub(crate) from_imports: HashMap<String, (String, String)>,
    /// local dotted name -> module (`s` -> `pipelines.sources`, `a.b` -> `a.b`)
    pub(crate) modules: HashMap<String, String>,
    /// The decorator and helper names that are positively barca's, for the argument check.
    pub(crate) barca: crate::decorator_args::BarcaNames,
}

impl FileNames {
    pub(crate) fn collect(body: &[Stmt]) -> Self {
        Self::with_barca(body, crate::decorator_args::BarcaNames::of(body))
    }

    pub(crate) fn with_barca(body: &[Stmt], barca: crate::decorator_args::BarcaNames) -> Self {
        let mut names = FileNames {
            barca,
            ..FileNames::default()
        };
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    names.local.insert(f.name.to_string());
                }
                Stmt::ImportFrom(imp) => {
                    let module = format!(
                        "{}{}",
                        ".".repeat(imp.level as usize),
                        imp.module.as_ref().map(|m| m.as_str()).unwrap_or("")
                    );
                    for alias in &imp.names {
                        let name = alias.name.to_string();
                        let local = alias
                            .asname
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| name.clone());
                        names.from_imports.insert(local, (module.clone(), name));
                    }
                }
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        let module = alias.name.to_string();
                        let local = alias
                            .asname
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| module.clone());
                        names.modules.insert(local, module);
                    }
                }
                _ => {}
            }
        }
        names
    }

    /// The node an `inputs=` value (or `collect(...)` / `partitions_from(...)` argument) refers
    /// to: a function in this file, a name imported from a module, or `module.name`.
    pub(crate) fn node_ref(&self, expr: &Expr) -> Option<NodeRef> {
        match expr {
            Expr::Name(n) => {
                let id = n.id.to_string();
                if self.local.contains(&id) {
                    return Some(NodeRef::FunctionName(id));
                }
                Some(match self.from_imports.get(&id) {
                    Some((module, name)) => NodeRef::Imported {
                        module: module.clone(),
                        name: name.clone(),
                    },
                    None => NodeRef::FunctionName(id),
                })
            }
            Expr::Attribute(attr) => {
                let dotted = dotted_expr(&attr.value)?;
                let module = self.modules.get(&dotted)?;
                Some(NodeRef::Imported {
                    module: module.clone(),
                    name: attr.attr.to_string(),
                })
            }
            _ => None,
        }
    }
}

/// `a.b.c` as a string, for a chain of names.
pub(crate) fn dotted_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(format!("{}.{}", dotted_expr(&a.value)?, a.attr)),
        _ => None,
    }
}
