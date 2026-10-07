//! Dependency cone: the helper code a step's function reaches, hashed.
//!
//! The cone is computed from source text alone (ruff's AST; nothing is imported), by these
//! rules. "Project module" means a module the [`ModuleSource`] resolves to a file of the
//! project; the standard library and installed packages are never project modules, are never
//! read, and add at most a constant marker.
//!
//! 1. **Definitions.** A module's definitions are its top-level `def`s, `class`es, assignments
//!    and imports ([`ModuleDef`]). A function contributes its body; a class contributes its
//!    bases, keywords and whole body (methods and class-level code).
//! 2. **Names.** Code that mentions the name of a definition of its own module adds that
//!    definition's source and, transitively, what it uses. Local scopes are *not* resolved for
//!    this rule: a parameter called `rate` counts as a use of a module-level `rate`. That can
//!    only add to the cone, and it is what every earlier release hashed.
//! 3. **`from module import name [as alias]`.** A use of the bound name adds `module`'s
//!    definition of `name`, following re-exports; a module that is not a project module adds a
//!    constant marker instead.
//! 4. **`import module [as alias]` + `alias.attr`.** Adds `module`'s definition of `attr`,
//!    exactly as rule 3 would (`pkg.mod.f` reads `f` from the longest prefix that is a module).
//! 5. **A module used as a value** (`getattr(helpers, n)`, `run(helpers)`): which attribute is
//!    read cannot be known, so the module's whole source and every definition in it are added.
//!    This rule does resolve local scopes: a parameter or local variable called `helpers` is
//!    not the module.
//! 6. **Imports inside a function or class body** bind names for that body and the scopes
//!    nested in it; uses of those names follow rules 3 to 5.
//!
//! The hash is over the sorted `(label, source)` pairs collected, so it does not depend on the
//! order definitions are visited in.

use ruff_python_ast::visitor::{self, Visitor};
use ruff_python_ast::{Comprehension, ExceptHandler, Expr, ExprContext, Pattern, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;
use sha2::{Digest, Sha256};
use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

/// How many project modules along an import chain are followed: a step that imports from `m1`,
/// which imports from `m2`, ... reaches `m6` and no further (`barca docs cache`: "more than six
/// project modules away"). It bounds re-export cycles; past it a name contributes only its
/// marker. Depths count from 0 (the module the pipeline file imports), so the limit is exclusive.
const MAX_MODULES_DEEP: usize = 6;

// ─── Modules ─────────────────────────────────────────────────────────────────

/// One Python source file under the name Python imports it by.
pub struct Module {
    /// Dotted import name (`helpers`, `pkg.mod`; `pkg` for `pkg/__init__.py`).
    pub name: String,
    pub source: Rc<str>,
    /// The package relative imports inside this module resolve against: its own name for an
    /// `__init__.py`, its parent package for a regular module, `None` at the top level.
    package: Option<String>,
    definitions: OnceCell<HashMap<String, ModuleDef>>,
}

impl Module {
    pub fn new(name: impl Into<String>, source: impl Into<Rc<str>>, is_package: bool) -> Self {
        let name = name.into();
        let package = if is_package {
            Some(name.clone())
        } else {
            name.rsplit_once('.').map(|(parent, _)| parent.to_string())
        };
        Module {
            name,
            source: source.into(),
            package,
            definitions: OnceCell::new(),
        }
    }

    /// The module's top-level definitions, parsed on first use and then kept.
    pub fn definitions(&self) -> &HashMap<String, ModuleDef> {
        self.definitions
            .get_or_init(|| collect_definitions(&self.source, self.package.as_deref()))
    }
}

/// Resolves a dotted module name to the project file Python would import for it.
pub trait ModuleSource {
    /// `None` when `name` is not a project module (standard library, installed package,
    /// namespace package, or nothing at all).
    fn module(&self, name: &str) -> Option<Rc<Module>>;
}

/// A fixed set of modules held in memory (tests, and callers with no project around them).
impl ModuleSource for HashMap<String, Rc<Module>> {
    fn module(&self, name: &str) -> Option<Rc<Module>> {
        self.get(name).cloned()
    }
}

// ─── Definitions ─────────────────────────────────────────────────────────────

/// What a top-level name of a module is bound to.
#[derive(Clone)]
pub enum ModuleDef {
    Function(Code),
    Class(Code),
    Assignment(Code),
    /// `from module import name [as alias]`: bound to `module`'s definition of `name`.
    FromImport {
        module: String,
        name: String,
    },
    /// `import module` / `import a.b as m`: bound to the module itself (`import a.b` binds
    /// `a`).
    ModuleImport {
        module: String,
    },
}

/// The source of a definition and what it refers to.
#[derive(Clone)]
pub struct Code {
    source_text: Rc<str>,
    uses: Uses,
}

/// What a piece of code refers to.
#[derive(Clone, Default)]
pub struct Uses {
    /// Every name mentioned, and every dotted chain rooted at a name with each of its prefixes
    /// (`a.b.c` records `a`, `a.b`, `a.b.c`). Local scopes are not resolved (rules 2 to 4).
    names: HashSet<String>,
    /// Names and dotted chains read as a whole, exactly as written (`helpers`, `pkg.mod`),
    /// whose root no local binding shadows (rule 5).
    values: BTreeSet<String>,
    /// Uses of names bound by an import inside the code itself (rule 6).
    local: Vec<LocalUse>,
}

/// `bound` (optionally `bound.attrs...`) where `bound` was imported inside the code.
#[derive(Clone)]
struct LocalUse {
    bound: String,
    /// Always `FromImport` or `ModuleImport`.
    binding: ModuleDef,
    attrs: Vec<String>,
}

// ─── The cone ────────────────────────────────────────────────────────────────

/// The dependency cone hash of `function_name` in `pipeline`. Empty when the function reaches
/// nothing (or is not a top-level function of the file).
pub fn cone_hash(pipeline: &Module, function_name: &str, modules: &dyn ModuleSource) -> String {
    let Some(ModuleDef::Function(code)) = pipeline.definitions().get(function_name) else {
        return String::new();
    };
    let mut cone = Cone {
        modules,
        visited: HashSet::new(),
        parts: Vec::new(),
    };
    let site = Site {
        module: pipeline,
        is_pipeline: true,
        depth: 0,
    };
    cone.follow(site, &code.uses);
    cone.hash()
}

/// The module whose definitions a use is resolved against.
#[derive(Clone, Copy)]
struct Site<'m> {
    module: &'m Module,
    /// The pipeline file's own definitions are labelled by bare name, an imported module's by
    /// `module:name`.
    is_pipeline: bool,
    /// Import depth of the modules this site imports from.
    depth: usize,
}

impl Site<'_> {
    fn label(&self, name: &str) -> String {
        if self.is_pipeline {
            name.to_string()
        } else {
            format!("{}:{name}", self.module.name)
        }
    }
}

/// What a name bound to a project module, plus the attribute chain read from it, uses.
enum ModuleUse {
    /// `module.attr`: one definition (rule 4).
    Attribute(String, String),
    /// The module itself (rule 5).
    Whole(String),
}

struct Cone<'a> {
    modules: &'a dyn ModuleSource,
    /// Labels already added through a name or an attribute, so each is added once.
    visited: HashSet<String>,
    /// `(label, source)` of everything in the cone.
    parts: Vec<(String, Rc<str>)>,
}

impl Cone<'_> {
    fn hash(mut self) -> String {
        if self.parts.is_empty() {
            return String::new();
        }
        self.parts.sort();
        let mut hasher = Sha256::new();
        for (label, text) in &self.parts {
            hasher.update(label.as_bytes());
            hasher.update(b":");
            hasher.update(text.as_bytes());
            hasher.update(b"\n");
        }
        format!("{:x}", hasher.finalize())
    }

    /// Add everything `uses` reaches from `site`, transitively.
    fn follow(&mut self, site: Site, uses: &Uses) {
        let definitions = site.module.definitions();
        let mut pending = vec![uses];
        while let Some(uses) = pending.pop() {
            self.follow_modules(site, uses);
            for name in &uses.names {
                let Some(def) = definitions.get(name) else {
                    continue;
                };
                // A module binding on its own adds nothing here: rules 4 and 5 decide.
                if matches!(def, ModuleDef::ModuleImport { .. })
                    || !self.visited.insert(site.label(name))
                {
                    continue;
                }
                match def {
                    ModuleDef::Function(code)
                    | ModuleDef::Class(code)
                    | ModuleDef::Assignment(code) => {
                        self.parts
                            .push((site.label(name), code.source_text.clone()));
                        pending.push(&code.uses);
                    }
                    ModuleDef::FromImport {
                        module,
                        name: original,
                    } => self.import(module, original, name, site.depth),
                    ModuleDef::ModuleImport { .. } => {}
                }
            }
        }
    }

    /// Rules 4 to 6: uses that go through a name bound to a module, or bound by an import
    /// inside the code.
    fn follow_modules(&mut self, site: Site, uses: &Uses) {
        let definitions = site.module.definitions();
        // What the root of a reference (`a` in `a.b.c`) is bound to at module level, and the
        // attributes read from it.
        fn binding_of<'d, 'r>(
            definitions: &'d HashMap<String, ModuleDef>,
            reference: &'r str,
        ) -> Option<(&'d ModuleDef, Vec<&'r str>)> {
            let mut segments = reference.split('.');
            let binding = definitions.get(segments.next()?)?;
            Some((binding, segments.collect()))
        }

        let mut attributes: Vec<(String, String)> = uses
            .names
            .iter()
            .filter(|reference| reference.contains('.'))
            .filter_map(|reference| {
                let (binding, attrs) = binding_of(definitions, reference)?;
                match self.module_use(binding, &attrs)? {
                    ModuleUse::Attribute(module, attr) => Some((module, attr)),
                    ModuleUse::Whole(_) => None,
                }
            })
            .collect();
        attributes.sort();
        attributes.dedup();
        for (module, attr) in attributes {
            self.attribute(&module, &attr, site.depth);
        }

        for reference in &uses.values {
            if let Some((binding, attrs)) = binding_of(definitions, reference)
                && let Some(ModuleUse::Whole(module)) = self.module_use(binding, &attrs)
            {
                self.whole_module(&module, site.depth);
            }
        }

        for local in &uses.local {
            match self.module_use(&local.binding, &local.attrs) {
                Some(ModuleUse::Attribute(module, attr)) => {
                    self.attribute(&module, &attr, site.depth)
                }
                Some(ModuleUse::Whole(module)) => self.whole_module(&module, site.depth),
                None => {
                    if let ModuleDef::FromImport { module, name } = &local.binding
                        && self.modules.module(module).is_some()
                        && self.visited.insert(format!("{module}:{name}"))
                    {
                        self.import(module, name, &local.bound, site.depth);
                    }
                }
            }
        }
    }

    /// What `binding.attrs...` uses, when `binding` is bound to a project module. `None` for
    /// anything else: a name that is not a module, or a module outside the project
    /// (`json.dumps`).
    fn module_use<S: AsRef<str>>(&self, binding: &ModuleDef, attrs: &[S]) -> Option<ModuleUse> {
        let is_module = |name: &str| self.modules.module(name).is_some();
        let base = match binding {
            ModuleDef::ModuleImport { module } => module.clone(),
            // `from pkg import mod`, where `pkg.mod` is itself a module.
            ModuleDef::FromImport { module, name } => {
                let full = join_dotted(module, &[name]);
                if !is_module(&full) {
                    return None;
                }
                full
            }
            _ => return None,
        };
        // The whole chain names a module (`helpers`, `pkg.mod`): it is used as a value.
        let whole = join_dotted(&base, attrs);
        if is_module(&whole) {
            return Some(ModuleUse::Whole(whole));
        }
        // Otherwise the longest prefix that is a module; the next segment is the attribute read
        // from it (`pkg.mod.f.x` reads `f` from `pkg.mod`).
        (0..attrs.len()).rev().find_map(|k| {
            let module = join_dotted(&base, &attrs[..k]);
            is_module(&module).then(|| ModuleUse::Attribute(module, attrs[k].as_ref().to_string()))
        })
    }

    /// Rule 4: `module.attr`, added once however many times it is read.
    fn attribute(&mut self, module: &str, attr: &str, depth: usize) {
        if self.visited.insert(format!("{module}:{attr}")) {
            self.import(module, attr, attr, depth);
        }
    }

    /// Rule 3: `module`'s definition of `name` and what it uses, following re-exports.
    /// `bound` is the name the importing code bound it to.
    ///
    /// Deliberately not deduplicated through `visited`: a definition imported here and also
    /// reached from inside its module (a recursive helper, say) is added twice. Every release
    /// since 0.10 has hashed it that way; adding it once would change those hashes and
    /// recompute their caches (`recursive_helper_hash_from_0_17_0_is_unchanged`).
    fn import(&mut self, module: &str, name: &str, bound: &str, depth: usize) {
        let found = (depth < MAX_MODULES_DEEP)
            .then(|| self.modules.module(module))
            .flatten();
        let def = found.as_ref().and_then(|m| m.definitions().get(name));
        match (found.as_deref(), def) {
            (
                Some(imported),
                Some(
                    ModuleDef::Function(code)
                    | ModuleDef::Class(code)
                    | ModuleDef::Assignment(code),
                ),
            ) => {
                self.parts
                    .push((format!("{module}:{name}"), code.source_text.clone()));
                let site = Site {
                    module: imported,
                    is_pipeline: false,
                    depth: depth + 1,
                };
                self.follow(site, &code.uses);
            }
            (
                Some(_),
                Some(ModuleDef::FromImport {
                    module: next,
                    name: original,
                }),
            ) => self.import(next, original, name, depth + 1),
            // Not a project module, or nothing barca can read `name` from there (defined
            // dynamically, a module binding, a star import): a constant marker for the binding,
            // so the hash still says the name is imported.
            _ => self
                .parts
                .push((bound.to_string(), format!("import:{module}:{bound}").into())),
        }
    }

    /// Rule 5: the module's whole source, and everything its definitions use.
    fn whole_module(&mut self, name: &str, depth: usize) {
        let Some(module) = self.modules.module(name) else {
            return;
        };
        if depth >= MAX_MODULES_DEEP || !self.visited.insert(format!("{name}:*")) {
            return;
        }
        self.parts
            .push((format!("{name}:*"), module.source.clone()));
        let every_definition = Uses {
            names: module.definitions().keys().cloned().collect(),
            ..Uses::default()
        };
        let site = Site {
            module: &module,
            is_pipeline: false,
            depth: depth + 1,
        };
        self.follow(site, &every_definition);
    }
}

/// `base` + `segments`, dotted; an empty `base` (a bare `from . import x`) is skipped.
fn join_dotted<S: AsRef<str>>(base: &str, segments: &[S]) -> String {
    let mut dotted = base.to_string();
    for segment in segments {
        if !dotted.is_empty() {
            dotted.push('.');
        }
        dotted.push_str(segment.as_ref());
    }
    dotted
}

// ─── Collecting definitions ──────────────────────────────────────────────────

/// Rule 1: the top-level definitions of a module. `package` is what its relative imports
/// resolve against (see [`Module`]). Source that does not parse has no definitions.
fn collect_definitions(source: &str, package: Option<&str>) -> HashMap<String, ModuleDef> {
    let Ok(parsed) = parse_module(source) else {
        return HashMap::new();
    };
    let text = |node: &dyn Ranged| -> Rc<str> {
        source[node.range().start().to_usize()..node.range().end().to_usize()].into()
    };
    let code = |node: &dyn Ranged, uses: UseCollector| Code {
        source_text: text(node),
        uses: uses.uses,
    };
    let mut defs: HashMap<String, ModuleDef> = HashMap::new();

    for stmt in &parsed.syntax().body {
        match stmt {
            Stmt::FunctionDef(func) => {
                let mut uses = UseCollector::new(package);
                uses.function(func);
                defs.insert(func.name.to_string(), ModuleDef::Function(code(func, uses)));
            }
            Stmt::ClassDef(class) => {
                let mut uses = UseCollector::new(package);
                for keyword in class.arguments.iter().flat_map(|a| a.keywords.iter()) {
                    uses.expr(&keyword.value);
                }
                uses.class(class);
                defs.insert(class.name.to_string(), ModuleDef::Class(code(class, uses)));
            }
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Expr::Name(n) = target {
                        let mut uses = UseCollector::new(package);
                        uses.expr(&assign.value);
                        defs.insert(n.id.to_string(), ModuleDef::Assignment(code(assign, uses)));
                    }
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Expr::Name(n) = assign.target.as_ref() {
                    let mut uses = UseCollector::new(package);
                    if let Some(value) = &assign.value {
                        uses.expr(value);
                    }
                    defs.insert(n.id.to_string(), ModuleDef::Assignment(code(assign, uses)));
                }
            }
            // In Python the later binding of a name wins. Here `import name` does not replace
            // an earlier definition of `name` (a `from` import does): that is what 0.17 hashed,
            // and making it Python's would change the hash of a file that defines a name and
            // later imports a module of the same name (`import_does_not_replace_...` pins it).
            Stmt::Import(_) => {
                for (bound, def) in import_bindings(stmt, package) {
                    defs.entry(bound).or_insert(def);
                }
            }
            Stmt::ImportFrom(_) => defs.extend(import_bindings(stmt, package)),
            _ => {}
        }
    }
    defs
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

/// The names an `import` / `from ... import` statement binds, and what each is bound to.
/// `import a.b` binds `a`; `import a.b as m` binds `m` to `a.b`; `from m import f as g` binds
/// `g` to `m`'s `f`. `from .core import x` (level 1) resolves against `package`; each extra
/// leading dot walks one more package level up.
fn import_bindings(stmt: &Stmt, package: Option<&str>) -> Vec<(String, ModuleDef)> {
    match stmt {
        Stmt::Import(import) => import
            .names
            .iter()
            .map(|alias| {
                let full = alias.name.to_string();
                match &alias.asname {
                    Some(asname) => (asname.to_string(), ModuleDef::ModuleImport { module: full }),
                    None => {
                        let top = full.split('.').next().unwrap_or_default().to_string();
                        (top.clone(), ModuleDef::ModuleImport { module: top })
                    }
                }
            })
            .collect(),
        Stmt::ImportFrom(import) => {
            let written = import
                .module
                .as_ref()
                .map(|m| m.to_string())
                .unwrap_or_default();
            let module = if import.level > 0 {
                match package.and_then(|pkg| {
                    package_ancestor(pkg, (import.level as usize).saturating_sub(1))
                }) {
                    Some(base) if written.is_empty() => base,
                    Some(base) => format!("{base}.{written}"),
                    None => written,
                }
            } else {
                written
            };
            import
                .names
                .iter()
                .map(|alias| {
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name).to_string();
                    let def = ModuleDef::FromImport {
                        module: module.clone(),
                        name: alias.name.to_string(),
                    };
                    (bound, def)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

// ─── Local scopes ────────────────────────────────────────────────────────────

/// The names one scope binds (a function, a class body, a lambda, a comprehension). A name
/// bound by an import inside the scope carries what it was bound to; any other binding (a
/// parameter, an assignment, a loop variable, ...) carries nothing and only shadows.
#[derive(Default)]
struct Scope {
    bindings: HashMap<String, Vec<ModuleDef>>,
    /// A class body's names are visible to its own statements, not to the methods inside it.
    is_class: bool,
}

/// Finds the names a block of statements binds in its own scope, without entering the scopes
/// nested in it.
struct Binder<'p> {
    package: Option<&'p str>,
    scope: Scope,
    /// Declared `global` / `nonlocal`: assigned here, but not local.
    not_local: Vec<String>,
}

impl<'p> Binder<'p> {
    fn new(package: Option<&'p str>) -> Self {
        Binder {
            package,
            scope: Scope::default(),
            not_local: Vec::new(),
        }
    }

    fn bind(&mut self, name: &str) {
        self.scope.bindings.entry(name.to_string()).or_default();
    }

    fn finish(mut self) -> Scope {
        for name in &self.not_local {
            self.scope.bindings.remove(name);
        }
        self.scope
    }
}

impl<'a> Visitor<'a> for Binder<'_> {
    fn visit_stmt(&mut self, stmt: &'a Stmt) {
        match stmt {
            Stmt::FunctionDef(func) => self.bind(&func.name),
            Stmt::ClassDef(class) => self.bind(&class.name),
            Stmt::Import(_) | Stmt::ImportFrom(_) => {
                for (bound, def) in import_bindings(stmt, self.package) {
                    self.scope.bindings.entry(bound).or_default().push(def);
                }
            }
            Stmt::Global(s) => self.not_local.extend(s.names.iter().map(|n| n.to_string())),
            Stmt::Nonlocal(s) => self.not_local.extend(s.names.iter().map(|n| n.to_string())),
            _ => visitor::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Name(name) if !matches!(name.ctx, ExprContext::Load) => self.bind(&name.id),
            Expr::Lambda(_) => {}
            _ => visitor::walk_expr(self, expr),
        }
    }

    /// A comprehension's targets belong to the comprehension, not to the enclosing scope.
    fn visit_comprehension(&mut self, comprehension: &'a Comprehension) {
        self.visit_expr(&comprehension.iter);
        for condition in &comprehension.ifs {
            self.visit_expr(condition);
        }
    }

    fn visit_except_handler(&mut self, handler: &'a ExceptHandler) {
        let ExceptHandler::ExceptHandler(h) = handler;
        if let Some(name) = &h.name {
            self.bind(name);
        }
        visitor::walk_except_handler(self, handler);
    }

    fn visit_pattern(&mut self, pattern: &'a Pattern) {
        let captured = match pattern {
            Pattern::MatchAs(p) => p.name.as_ref(),
            Pattern::MatchStar(p) => p.name.as_ref(),
            Pattern::MatchMapping(p) => p.rest.as_ref(),
            _ => None,
        };
        if let Some(name) = captured {
            self.bind(name);
        }
        visitor::walk_pattern(self, pattern);
    }
}

// ─── Collecting uses ─────────────────────────────────────────────────────────

/// Collects the [`Uses`] of one definition, tracking the local scopes it opens.
struct UseCollector<'p> {
    package: Option<&'p str>,
    uses: Uses,
    scopes: Vec<Scope>,
}

impl<'p> UseCollector<'p> {
    fn new(package: Option<&'p str>) -> Self {
        UseCollector {
            package,
            uses: Uses::default(),
            scopes: Vec::new(),
        }
    }

    /// The local binding `name` refers to here, if any: the innermost scope that binds it,
    /// skipping class bodies we are no longer directly inside.
    fn local(&self, name: &str) -> Option<&[ModuleDef]> {
        let innermost = self.scopes.len().saturating_sub(1);
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .filter(|(i, scope)| !scope.is_class || *i == innermost)
            .find_map(|(_, scope)| scope.bindings.get(name))
            .map(Vec::as_slice)
    }

    /// `root` or `root.attrs...` is read, as written.
    fn reference(&mut self, root: &str, attrs: Vec<String>) {
        match self.local(root) {
            None => {
                self.uses.values.insert(join_dotted(root, &attrs));
            }
            Some(imports) => {
                let uses = imports.iter().map(|binding| LocalUse {
                    bound: root.to_string(),
                    binding: binding.clone(),
                    attrs: attrs.clone(),
                });
                self.uses.local.extend(uses.collect::<Vec<_>>());
            }
        }
    }

    fn scoped(&mut self, scope: Scope, walk: impl FnOnce(&mut Self)) {
        self.scopes.push(scope);
        walk(self);
        self.scopes.pop();
    }

    fn block_scope(
        &self,
        parameters: Option<&ruff_python_ast::Parameters>,
        body: &[Stmt],
    ) -> Scope {
        let mut binder = Binder::new(self.package);
        for parameter in parameters.into_iter().flat_map(|p| p.iter()) {
            binder.bind(parameter.name());
        }
        binder.visit_body(body);
        binder.finish()
    }

    /// A function contributes its body (not its decorators, defaults or annotations).
    fn function(&mut self, func: &ruff_python_ast::StmtFunctionDef) {
        let scope = self.block_scope(Some(&func.parameters), &func.body);
        self.scoped(scope, |c| c.stmts(&func.body));
    }

    /// A class contributes its bases and its body.
    fn class(&mut self, class: &ruff_python_ast::StmtClassDef) {
        for base in class.arguments.iter().flat_map(|a| a.args.iter()) {
            self.expr(base);
        }
        let mut scope = self.block_scope(None, &class.body);
        scope.is_class = true;
        self.scoped(scope, |c| c.stmts(&class.body));
    }

    fn comprehension(&mut self, results: &[&Expr], generators: &[Comprehension]) {
        let mut binder = Binder::new(self.package);
        for generator in generators {
            binder.visit_expr(&generator.target);
        }
        self.scoped(binder.finish(), |c| {
            c.exprs(results.iter().copied());
            for generator in generators {
                c.expr(&generator.iter);
                c.exprs(&generator.ifs);
            }
        });
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    fn exprs<'e>(&mut self, exprs: impl IntoIterator<Item = &'e Expr>) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    // The statements and expressions visited below are exactly the ones every earlier release
    // visited: visiting more (a decorator, a default value, an `except` type, ...) would add
    // names, and so change the hash of code that has not changed.
    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Expr(s) => self.expr(&s.value),
            Stmt::Return(s) => self.exprs(s.value.as_deref()),
            Stmt::Assign(s) => self.expr(&s.value),
            Stmt::AugAssign(s) => {
                self.expr(&s.target);
                self.expr(&s.value);
            }
            Stmt::AnnAssign(s) => self.exprs(s.value.as_deref()),
            Stmt::If(s) => {
                self.expr(&s.test);
                self.stmts(&s.body);
                for clause in &s.elif_else_clauses {
                    self.exprs(&clause.test);
                    self.stmts(&clause.body);
                }
            }
            Stmt::For(s) => {
                self.expr(&s.iter);
                self.stmts(&s.body);
            }
            Stmt::While(s) => {
                self.expr(&s.test);
                self.stmts(&s.body);
            }
            Stmt::With(s) => {
                self.exprs(s.items.iter().map(|item| &item.context_expr));
                self.stmts(&s.body);
            }
            Stmt::FunctionDef(s) => self.function(s),
            Stmt::ClassDef(s) => self.class(s),
            Stmt::Try(s) => {
                self.stmts(&s.body);
                for handler in &s.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    self.stmts(&handler.body);
                }
                self.stmts(&s.orelse);
                self.stmts(&s.finalbody);
            }
            Stmt::Match(s) => {
                self.expr(&s.subject);
                for case in &s.cases {
                    self.exprs(case.guard.as_deref());
                    self.stmts(&case.body);
                }
            }
            Stmt::Raise(s) => {
                self.exprs(s.exc.as_deref());
                self.exprs(s.cause.as_deref());
            }
            Stmt::Assert(s) => {
                self.expr(&s.test);
                self.exprs(s.msg.as_deref());
            }
            Stmt::Delete(s) => self.exprs(&s.targets),
            _ => {}
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Name(n) => {
                self.uses.names.insert(n.id.to_string());
                self.reference(&n.id, Vec::new());
            }
            Expr::Attribute(a) => match dotted_chain(expr) {
                // `helpers.compute`: the root, and the chain with each of its prefixes.
                Some((root, attrs)) => {
                    self.uses.names.insert(root.to_string());
                    for end in 1..=attrs.len() {
                        self.uses.names.insert(join_dotted(root, &attrs[..end]));
                    }
                    self.reference(root, attrs);
                }
                // `f().x`: whatever the value uses.
                None => self.expr(&a.value),
            },
            Expr::Call(c) => {
                self.expr(&c.func);
                self.exprs(c.arguments.args.iter());
                self.exprs(c.arguments.keywords.iter().map(|kw| &kw.value));
            }
            Expr::Subscript(s) => {
                self.expr(&s.value);
                self.expr(&s.slice);
            }
            Expr::BinOp(b) => {
                self.expr(&b.left);
                self.expr(&b.right);
            }
            Expr::UnaryOp(u) => self.expr(&u.operand),
            Expr::BoolOp(b) => self.exprs(&b.values),
            Expr::Compare(c) => {
                self.expr(&c.left);
                self.exprs(c.comparators.iter());
            }
            Expr::If(i) => {
                self.expr(&i.test);
                self.expr(&i.body);
                self.expr(&i.orelse);
            }
            Expr::Dict(d) => {
                for item in &d.items {
                    self.exprs(&item.key);
                    self.expr(&item.value);
                }
            }
            Expr::List(l) => self.exprs(&l.elts),
            Expr::Tuple(t) => self.exprs(&t.elts),
            Expr::Lambda(l) => {
                let scope = self.block_scope(l.parameters.as_deref(), &[]);
                self.scoped(scope, |c| c.expr(&l.body));
            }
            Expr::Starred(s) => self.expr(&s.value),
            Expr::ListComp(c) => self.comprehension(&[&c.elt], &c.generators),
            Expr::SetComp(c) => self.comprehension(&[&c.elt], &c.generators),
            Expr::Generator(g) => self.comprehension(&[&g.elt], &g.generators),
            Expr::DictComp(c) => {
                let results: Vec<&Expr> = c.key.iter().chain([&c.value]).map(|e| &**e).collect();
                self.comprehension(&results, &c.generators);
            }
            Expr::FString(f) => {
                for part in &f.value {
                    if let ruff_python_ast::FStringPart::FString(fstr) = part {
                        for interpolation in fstr.elements.interpolations() {
                            self.expr(&interpolation.expression);
                        }
                    }
                }
            }
            Expr::Named(n) => self.expr(&n.value),
            Expr::Await(a) => self.expr(&a.value),
            Expr::Yield(y) => self.exprs(y.value.as_deref()),
            Expr::YieldFrom(y) => self.expr(&y.value),
            Expr::Slice(s) => {
                self.exprs(s.lower.as_deref());
                self.exprs(s.upper.as_deref());
                self.exprs(s.step.as_deref());
            }
            _ => {}
        }
    }
}

/// (`a`, [`b`, `c`]) for an attribute chain `a.b.c` rooted at a plain name; `None` otherwise
/// (`f().x`).
fn dotted_chain(expr: &Expr) -> Option<(&str, Vec<String>)> {
    match expr {
        Expr::Name(n) => Some((n.id.as_str(), Vec::new())),
        Expr::Attribute(a) => {
            let (root, mut attrs) = dotted_chain(&a.value)?;
            attrs.push(a.attr.to_string());
            Some((root, attrs))
        }
        _ => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory project modules; `packages` are the names that are `__init__.py` packages.
    fn sources(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn one(name: &str, src: &str) -> HashMap<String, String> {
        sources(&[(name, src)])
    }

    /// The cone hash of `function` in a pipeline file loaded by path (no package).
    fn hash_fn(
        entry: &str,
        function: &str,
        other: &HashMap<String, String>,
        packages: &[&str],
    ) -> String {
        let modules: HashMap<String, Rc<Module>> = other
            .iter()
            .map(|(name, src)| {
                let module = Module::new(
                    name.clone(),
                    src.as_str(),
                    packages.contains(&name.as_str()),
                );
                (name.clone(), Rc::new(module))
            })
            .collect();
        super::cone_hash(&Module::new("pipeline", entry, false), function, &modules)
    }

    fn hash_of(entry: &str, other: &HashMap<String, String>, packages: &[&str]) -> String {
        hash_fn(entry, "my_asset", other, packages)
    }

    /// A pipeline file with no project modules around it.
    fn cone_hash(source: &str, function: &str) -> String {
        hash_fn(source, function, &HashMap::new(), &[])
    }

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

        let packages = ["mylib"];

        let h1 = hash_of(entry_src, &other_sources, &packages);

        // If `.util` had wrongly resolved to `mylib.core.util`, this lookup
        // would miss `other_sources` entirely and the cone hash would stay
        // constant regardless of `h`'s actual source.
        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = hash_of(entry_src, &other_sources, &packages);

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

        let packages = ["mylib", "mylib.sub"];

        let h1 = hash_of(entry_src, &other_sources, &packages);

        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = hash_of(entry_src, &other_sources, &packages);

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

        let packages = ["mylib"];

        let h1 = hash_of(entry_src, &other_sources, &packages);

        other_sources.insert("mylib.core".to_string(), core_src_v2.to_string());
        let h2 = hash_of(entry_src, &other_sources, &packages);

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

        let h1 = hash_of(entry_src, &other_sources, &[]);

        other_sources.insert("mylib.util".to_string(), util_src_v2.to_string());
        let h2 = hash_of(entry_src, &other_sources, &[]);

        assert_ne!(
            h1, h2,
            "a helper's transitive dependency that is itself a cross-file import must be tracked"
        );
    }

    // ─── `import module` + `module.attr` (#178) ──────────────────────────────

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

    // ─── Classes, in-function imports, modules used as values (#194) ────────────────

    const MODEL_V1: &str = "class Model:\n    def predict(self):\n        return 1\n\n\nclass Other:\n    def predict(self):\n        return 0\n";
    const MODEL_V2: &str = "class Model:\n    def predict(self):\n        return 2\n\n\nclass Other:\n    def predict(self):\n        return 0\n";
    const MODEL_UNRELATED: &str = "class Model:\n    def predict(self):\n        return 1\n\n\nclass Other:\n    def predict(self):\n        return 5\n";

    #[test]
    fn test_class_body_edit_in_helper_module_changes_hash() {
        let entry = "from helpers import Model\n\ndef my_asset():\n    return Model().predict()\n";
        let h1 = hash_of(entry, &one("helpers", MODEL_V1), &[]);
        assert!(!h1.is_empty());
        assert_ne!(h1, hash_of(entry, &one("helpers", MODEL_V2), &[]));
        assert_eq!(h1, hash_of(entry, &one("helpers", MODEL_UNRELATED), &[]));
    }

    #[test]
    fn test_class_defined_in_the_same_file_is_tracked() {
        let a =
            "class M:\n    def f(self):\n        return 1\n\ndef my_asset():\n    return M().f()\n";
        let b = a.replace("return 1", "return 2");
        assert_ne!(
            hash_of(a, &HashMap::new(), &[]),
            hash_of(&b, &HashMap::new(), &[])
        );
    }

    #[test]
    fn test_class_method_calling_another_helper_is_followed() {
        let entry = "from helpers import Model\n\ndef my_asset():\n    return Model().predict()\n";
        let m = "def base():\n    return 1\n\n\nclass Model:\n    def predict(self):\n        return base()\n";
        let h1 = hash_of(entry, &one("helpers", m), &[]);
        let h2 = hash_of(
            entry,
            &one("helpers", &m.replace("return 1", "return 2")),
            &[],
        );
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_import_inside_function_body_is_tracked() {
        let entry = "def my_asset():\n    from helpers import compute\n    return compute()\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert!(!h1.is_empty());
        assert_ne!(h1, hash_of(entry, &one("helpers", HELPERS_V2), &[]));
        assert_eq!(
            h1,
            hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[])
        );
    }

    #[test]
    fn test_module_import_inside_function_body_is_tracked() {
        let entry = "def my_asset():\n    import helpers\n    return helpers.compute()\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert_ne!(h1, hash_of(entry, &one("helpers", HELPERS_V2), &[]));
        assert_eq!(
            h1,
            hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[])
        );
    }

    #[test]
    fn test_import_inside_nested_block_and_method_is_tracked() {
        let entry = "class K:\n    def run(self):\n        if True:\n            from helpers import compute\n        return compute()\n\ndef my_asset():\n    return K().run()\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert_ne!(h1, hash_of(entry, &one("helpers", HELPERS_V2), &[]));
    }

    #[test]
    fn test_stdlib_import_inside_function_adds_nothing() {
        let entry = "def my_asset():\n    import json\n    return json.dumps(1)\n";
        assert!(hash_of(entry, &HashMap::new(), &[]).is_empty());
    }

    #[test]
    fn test_module_used_as_a_value_hashes_the_whole_module() {
        let entry = "import helpers\n\ndef my_asset(name):\n    return getattr(helpers, name)()\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert!(!h1.is_empty());
        assert_ne!(h1, hash_of(entry, &one("helpers", HELPERS_V2), &[]));
        // Conservative: an edit to any definition in the module counts.
        assert_ne!(
            h1,
            hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[])
        );
    }

    #[test]
    fn test_module_used_as_value_does_not_touch_other_modules() {
        let entry = "import helpers\n\ndef my_asset(name):\n    return getattr(helpers, name)()\n";
        let mut other = one("helpers", HELPERS_V1);
        other.insert(
            "elsewhere".to_string(),
            "def x():\n    return 1\n".to_string(),
        );
        let h1 = hash_of(entry, &other, &[]);
        other.insert(
            "elsewhere".to_string(),
            "def x():\n    return 2\n".to_string(),
        );
        assert_eq!(h1, hash_of(entry, &other, &[]));
    }

    #[test]
    fn test_attribute_use_of_a_module_stays_precise() {
        let entry = "import helpers\n\ndef my_asset():\n    return helpers.compute()\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert_eq!(
            h1,
            hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[])
        );
    }

    #[test]
    fn test_whole_module_follows_what_its_definitions_import() {
        let entry = "import helpers\n\ndef my_asset(name):\n    return getattr(helpers, name)()\n";
        let helpers = "from deep import inner\n\ndef compute():\n    return inner()\n";
        let deep =
            |n: u32| format!("def inner():\n    return {n}\n\n\ndef unused():\n    return 0\n");
        let h1 = hash_of(
            entry,
            &sources(&[("helpers", helpers), ("deep", &deep(1))]),
            &[],
        );
        let h2 = hash_of(
            entry,
            &sources(&[("helpers", helpers), ("deep", &deep(2))]),
            &[],
        );
        assert_ne!(h1, h2);
        // `deep` itself is not used as a value: only what `helpers` uses from it counts.
        let unused_edit = deep(1).replace("return 0", "return 9");
        assert_eq!(
            h1,
            hash_of(
                entry,
                &sources(&[("helpers", helpers), ("deep", &unused_edit)]),
                &[]
            )
        );
    }

    /// Every way a module can be handed around whole.
    #[test]
    fn test_every_form_of_module_value_hashes_the_whole_module() {
        let pkg = |unused: u32| {
            sources(&[
                (
                    "helpers",
                    &HELPERS_V1.replace("return 0", &format!("return {unused}")),
                ),
                ("pkg", ""),
                (
                    "pkg.mod",
                    &HELPERS_V1.replace("return 0", &format!("return {unused}")),
                ),
            ])
        };
        for entry in [
            "import helpers\n\ndef my_asset():\n    return run(helpers)\n",
            "import helpers\n\ndef my_asset():\n    m = helpers\n    return m.compute()\n",
            "import helpers as h\n\ndef my_asset():\n    return [h][0].compute()\n",
            "import helpers\n\nREGISTRY = {'h': helpers}\n\ndef my_asset():\n    return REGISTRY['h'].compute()\n",
            "from pkg import mod\n\ndef my_asset():\n    return run(mod)\n",
            "import pkg.mod\n\ndef my_asset():\n    return run(pkg.mod)\n",
            "import pkg.mod as m\n\ndef my_asset():\n    return vars(m)\n",
            "def my_asset():\n    import helpers\n    return getattr(helpers, 'compute')()\n",
            "def my_asset():\n    from pkg import mod as m\n    return run(m)\n",
            "def my_asset():\n    import helpers\n    return [getattr(helpers, n) for n in ('compute',)]\n",
            "def my_asset():\n    import helpers\n    def inner():\n        return run(helpers)\n    return inner()\n",
        ] {
            let h1 = hash_of(entry, &pkg(0), &["pkg"]);
            assert!(!h1.is_empty(), "nothing tracked for:\n{entry}");
            assert_ne!(
                h1,
                hash_of(entry, &pkg(5), &["pkg"]),
                "an edit anywhere in the module must count:\n{entry}"
            );
        }
    }

    /// A name that only *looks* like the module is not the module: the hash must not move when
    /// the module changes, and must be what it was before modules-as-values were tracked.
    #[test]
    fn test_a_local_binding_that_shares_a_module_name_is_not_a_module_value() {
        for (what, body) in [
            (
                "parameter",
                "def my_asset(helpers):\n    return run(helpers)\n",
            ),
            (
                "keyword-only parameter",
                "def my_asset(*, helpers=None):\n    return run(helpers)\n",
            ),
            (
                "*args",
                "def my_asset(*helpers):\n    return run(helpers)\n",
            ),
            (
                "**kwargs",
                "def my_asset(**helpers):\n    return run(helpers)\n",
            ),
            (
                "local assignment",
                "def my_asset():\n    helpers = [1]\n    return run(helpers)\n",
            ),
            (
                "assignment after the use",
                "def my_asset():\n    out = run(helpers)\n    helpers = [1]\n    return out\n",
            ),
            (
                "annotated assignment",
                "def my_asset():\n    helpers: list = [1]\n    return run(helpers)\n",
            ),
            (
                "augmented assignment",
                "def my_asset():\n    helpers += 1\n    return run(helpers)\n",
            ),
            (
                "tuple unpacking",
                "def my_asset():\n    a, (helpers, b) = 1, (2, 3)\n    return run(helpers)\n",
            ),
            (
                "for target",
                "def my_asset():\n    for helpers in range(3):\n        run(helpers)\n",
            ),
            (
                "with target",
                "def my_asset():\n    with open('f') as helpers:\n        return run(helpers)\n",
            ),
            (
                "except target",
                "def my_asset():\n    try:\n        pass\n    except Exception as helpers:\n        return run(helpers)\n",
            ),
            (
                "walrus",
                "def my_asset():\n    if (helpers := 3):\n        return run(helpers)\n",
            ),
            (
                "match capture",
                "def my_asset(x):\n    match x:\n        case [helpers, *_]:\n            return run(helpers)\n",
            ),
            (
                "nested def",
                "def my_asset():\n    def helpers():\n        return 1\n    return run(helpers)\n",
            ),
            (
                "nested class",
                "def my_asset():\n    class helpers:\n        pass\n    return run(helpers)\n",
            ),
            (
                "list comprehension variable",
                "def my_asset():\n    return [run(helpers) for helpers in range(3)]\n",
            ),
            (
                "dict comprehension variable",
                "def my_asset():\n    return {helpers: run(helpers) for helpers in range(3)}\n",
            ),
            (
                "generator variable",
                "def my_asset():\n    return sum(run(helpers) for helpers in range(3))\n",
            ),
            (
                "lambda parameter",
                "def my_asset():\n    return (lambda helpers: run(helpers))(1)\n",
            ),
            (
                "nested function parameter",
                "def my_asset():\n    def inner(helpers):\n        return run(helpers)\n    return inner(1)\n",
            ),
            (
                "enclosing function's local",
                "def my_asset():\n    helpers = 1\n    def inner():\n        return run(helpers)\n    return inner()\n",
            ),
        ] {
            let entry = format!("import helpers\n\n{body}");
            let h1 = hash_of(&entry, &one("helpers", HELPERS_V1), &[]);
            assert_eq!(
                h1,
                hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
                "{what} was taken for the module"
            );
            assert_eq!(h1, "", "{what}: nothing but the shadowed name is used");
        }
    }

    #[test]
    fn test_scopes_end_where_python_ends_them() {
        let v1 = one("helpers", HELPERS_V1);
        let edited = one("helpers", HELPERS_UNRELATED_EDIT);
        for (what, body) in [
            // A comprehension variable is gone after the comprehension.
            (
                "after a comprehension",
                "def my_asset():\n    xs = [helpers for helpers in range(3)]\n    return run(xs, helpers)\n",
            ),
            // A lambda's parameter is local to the lambda.
            (
                "beside a lambda",
                "def my_asset():\n    f = lambda helpers: helpers\n    return f(helpers)\n",
            ),
            // A nested function's parameter is local to it.
            (
                "beside a nested function",
                "def my_asset():\n    def inner(helpers):\n        return helpers\n    return inner(helpers)\n",
            ),
            // A class attribute is not visible inside the class's methods.
            (
                "inside a method",
                "class K:\n    helpers = 1\n    def m(self):\n        return run(helpers)\n\ndef my_asset():\n    return K().m()\n",
            ),
            // `global` makes the assignment a module-level one.
            (
                "declared global",
                "def my_asset():\n    global helpers\n    run(helpers)\n    helpers = None\n",
            ),
        ] {
            let entry = format!("import helpers\n\n{body}");
            assert_ne!(
                hash_of(&entry, &v1, &[]),
                hash_of(&entry, &edited, &[]),
                "{what}: the module itself is used"
            );
        }
    }

    #[test]
    fn test_module_attribute_stays_precise_next_to_a_shadowing_local() {
        let entry = "import helpers\n\ndef my_asset():\n    f = lambda helpers: run(helpers)\n    return f(helpers.compute())\n";
        let h1 = hash_of(entry, &one("helpers", HELPERS_V1), &[]);
        assert_eq!(
            h1,
            hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[])
        );
        assert_ne!(h1, hash_of(entry, &one("helpers", HELPERS_V2), &[]));
    }

    // ─── Classes (#194) ──────────────────────────────────────────────────────────

    #[test]
    fn test_base_class_metaclass_and_class_level_code_are_followed() {
        let entry = "from helpers import Model\n\ndef my_asset():\n    return Model().predict()\n";
        let module = |base: u32, meta: u32, rate: u32, unused: u32| {
            sources(&[
                ("helpers", "from bases import Base\n\nRATE = {RATE}\n\n\nclass Meta(type):\n    x = {META}\n\n\nclass Model(Base, metaclass=Meta):\n    rate = RATE\n\n    def predict(self):\n        return self.rate\n"
                    .replace("{RATE}", &rate.to_string()).replace("{META}", &meta.to_string()).as_str()),
                ("bases", &format!("class Base:\n    def fit(self):\n        return {base}\n\n\nclass Unused:\n    x = {unused}\n")),
            ])
        };
        let h1 = hash_of(entry, &module(1, 1, 1, 1), &[]);
        assert_ne!(
            h1,
            hash_of(entry, &module(2, 1, 1, 1), &[]),
            "base class in another module"
        );
        assert_ne!(h1, hash_of(entry, &module(1, 2, 1, 1), &[]), "metaclass");
        assert_ne!(
            h1,
            hash_of(entry, &module(1, 1, 2, 1), &[]),
            "constant used at class level"
        );
        assert_eq!(
            h1,
            hash_of(entry, &module(1, 1, 1, 2), &[]),
            "a class nothing uses"
        );
    }

    #[test]
    fn test_class_reached_through_a_module_attribute_is_tracked() {
        let entry = "import helpers\n\ndef my_asset():\n    return helpers.Model().predict()\n";
        let h1 = hash_of(entry, &one("helpers", MODEL_V1), &[]);
        assert_ne!(h1, hash_of(entry, &one("helpers", MODEL_V2), &[]));
        assert_eq!(h1, hash_of(entry, &one("helpers", MODEL_UNRELATED), &[]));
        let from = "from helpers import Model\n\ndef my_asset():\n    return Model().predict()\n";
        assert_eq!(h1, hash_of(from, &one("helpers", MODEL_V1), &[]));
    }

    // ─── Aliases (#194) ──────────────────────────────────────────────────────────

    /// `from m import f as g` and `import m as x`, at module level and inside the function:
    /// each hashes exactly what the unaliased module-level import hashes.
    #[test]
    fn test_aliased_imports_hash_like_unaliased_ones() {
        let v1 = one("helpers", HELPERS_V1);
        let plain = "from helpers import compute\n\ndef my_asset():\n    return compute()\n";
        let expected = hash_of(plain, &v1, &[]);
        assert!(!expected.is_empty());
        for entry in [
            "from helpers import compute as c\n\ndef my_asset():\n    return c()\n",
            "import helpers as h\n\ndef my_asset():\n    return h.compute()\n",
            "def my_asset():\n    from helpers import compute as c\n    return c()\n",
            "def my_asset():\n    import helpers as h\n    return h.compute()\n",
            "def my_asset():\n    from helpers import compute\n    return compute()\n",
            "def my_asset():\n    import helpers\n    return helpers.compute()\n",
        ] {
            assert_eq!(hash_of(entry, &v1, &[]), expected, "{entry}");
            assert_ne!(
                hash_of(entry, &one("helpers", HELPERS_V2), &[]),
                expected,
                "{entry}"
            );
            assert_eq!(
                hash_of(entry, &one("helpers", HELPERS_UNRELATED_EDIT), &[]),
                expected,
                "{entry}"
            );
        }
    }

    #[test]
    fn test_alias_in_a_re_export_chain_is_followed() {
        let entry = "from pkg import run_it as go\n\ndef my_asset():\n    return go()\n";
        let project = |n: u32| {
            sources(&[
                ("pkg", "from .core import compute as run_it\n"),
                ("pkg.core", &format!("def compute():\n    return {n}\n")),
            ])
        };
        let h1 = hash_of(entry, &project(1), &["pkg"]);
        assert_ne!(h1, hash_of(entry, &project(2), &["pkg"]));
    }

    #[test]
    fn test_conditional_imports_inside_a_function_are_both_followed() {
        let entry = "def my_asset(fast):\n    if fast:\n        from quick import compute\n    else:\n        from helpers import compute\n    return compute()\n";
        let project = |a: &str, b: &str| sources(&[("quick", a), ("helpers", b)]);
        let h1 = hash_of(entry, &project(HELPERS_V1, HELPERS_V1), &[]);
        assert_ne!(h1, hash_of(entry, &project(HELPERS_V2, HELPERS_V1), &[]));
        assert_ne!(h1, hash_of(entry, &project(HELPERS_V1, HELPERS_V2), &[]));
    }

    #[test]
    fn test_import_inside_a_helper_function_in_another_module_is_followed() {
        let entry = "from helpers import compute\n\ndef my_asset():\n    return compute()\n";
        let helpers = "def compute():\n    from deep import inner as i\n    return i()\n";
        let project = |n: u32| {
            sources(&[
                ("helpers", helpers),
                ("deep", &format!("def inner():\n    return {n}\n")),
            ])
        };
        assert_ne!(
            hash_of(entry, &project(1), &[]),
            hash_of(entry, &project(2), &[])
        );
    }

    // ─── Known limitations, as `barca docs cache` lists them ─────────────────────

    /// Not followed today. Each of these is in the manual ("Not followed"); when one starts
    /// being followed, the hash of code using it changes: move it out of this test, out of the
    /// manual, and into the release notes.
    #[test]
    fn documented_limitations_are_not_followed() {
        let v1 = one("helpers", HELPERS_V1);
        let v2 = one("helpers", HELPERS_V2);
        for (what, entry) in [
            (
                "star import",
                "from helpers import *\n\ndef my_asset():\n    return compute()\n",
            ),
            (
                "importlib",
                "import importlib\n\ndef my_asset():\n    return importlib.import_module('helpers').compute()\n",
            ),
            (
                "__import__",
                "def my_asset():\n    return __import__('helpers').compute()\n",
            ),
            (
                "decorator",
                "from helpers import compute\n\ndef wrap(v):\n    return lambda f: f\n\n@wrap(compute())\ndef inner():\n    return 1\n\ndef my_asset():\n    return inner()\n",
            ),
            (
                "default argument",
                "from helpers import compute\n\ndef inner(x=compute()):\n    return x\n\ndef my_asset():\n    return inner()\n",
            ),
            (
                "set literal",
                "import helpers\n\ndef my_asset():\n    return {helpers.compute()}\n",
            ),
            (
                "loop else",
                "import helpers\n\ndef my_asset():\n    for _ in ():\n        pass\n    else:\n        return helpers.compute()\n",
            ),
            (
                "assignment target",
                "import helpers\n\ndef my_asset():\n    t = {}\n    t[helpers.compute()] = 1\n    return t\n",
            ),
            (
                "except type",
                "import helpers\n\ndef my_asset():\n    try:\n        return 1\n    except helpers.compute:\n        return 2\n",
            ),
            (
                "match pattern",
                "import helpers\n\ndef my_asset(x):\n    match x:\n        case helpers.compute:\n            return 1\n",
            ),
            (
                "module reached through another module",
                "from reexport import helpers\n\ndef my_asset():\n    return helpers.compute()\n",
            ),
        ] {
            let with_reexport = |mut m: HashMap<String, String>| {
                m.insert("reexport".into(), "import helpers\n".into());
                m
            };
            assert_eq!(
                hash_of(entry, &with_reexport(v1.clone()), &[]),
                hash_of(entry, &with_reexport(v2.clone()), &[]),
                "{what} is followed now: update `barca docs cache`"
            );
        }
        // A name defined below the top level of its module.
        let entry = "from helpers import compute\n\ndef my_asset():\n    return compute()\n";
        let nested = |n: u32| {
            one(
                "helpers",
                &format!("if True:\n    def compute():\n        return {n}\n"),
            )
        };
        assert_eq!(
            hash_of(entry, &nested(1), &[]),
            hash_of(entry, &nested(2), &[])
        );
    }

    #[test]
    fn module_level_tuple_and_augmented_assignments_are_not_tracked() {
        // Documented under "Not followed". Tracking either would add the constant to the cone
        // of every step that already uses one, changing hashes 0.17 computed.
        let tuple = |b: u32| format!("A, B = 1, {b}\n\ndef my_asset():\n    return B\n");
        assert_eq!(
            cone_hash(&tuple(2), "my_asset"),
            cone_hash(&tuple(3), "my_asset")
        );
        let augmented = |n: u32| format!("A = 1\nA += {n}\n\ndef my_asset():\n    return A\n");
        assert_eq!(
            cone_hash(&augmented(2), "my_asset"),
            cone_hash(&augmented(3), "my_asset")
        );
    }

    // ─── Hashing quirks kept on purpose ──────────────────────────────────────────

    /// A helper that calls itself is in the cone twice: once as the imported name, once as a
    /// name its own body uses. Odd, but it is what 0.17.0 hashed (the same value is pinned end
    /// to end in `python/tests/test_run_hash_golden.py`), so it stays.
    #[test]
    fn recursive_helper_hash_from_0_17_0_is_unchanged() {
        let helper = "def fact(n):\n    return 1 if n < 2 else n * fact(n - 1)";
        let entry = "from helpers import fact\n\ndef my_asset():\n    return fact(5)\n";
        let mut twice = Sha256::new();
        for _ in 0..2 {
            twice.update(format!("helpers:fact:{helper}\n").as_bytes());
        }
        assert_eq!(
            hash_of(entry, &one("helpers", &format!("{helper}\n")), &[]),
            format!("{:x}", twice.finalize())
        );
    }

    /// `import name` after a definition of `name` keeps the definition (Python would rebind
    /// the name to the module); a `from` import replaces it. Kept as 0.17 hashed it.
    #[test]
    fn import_does_not_replace_an_earlier_definition_of_the_same_name() {
        let entry = |n: u32| {
            format!(
                "def helpers():\n    return {n}\n\nimport helpers\n\ndef my_asset():\n    return helpers()\n"
            )
        };
        let project = one("helpers", HELPERS_V1);
        assert_ne!(
            hash_of(&entry(1), &project, &[]),
            hash_of(&entry(2), &project, &[])
        );
        // A `from` import does replace it: the function above it is no longer what is used.
        let from = |n: u32| {
            format!(
                "def compute():\n    return {n}\n\nfrom helpers import compute\n\ndef my_asset():\n    return compute()\n"
            )
        };
        assert_eq!(
            hash_of(&from(1), &project, &[]),
            hash_of(&from(2), &project, &[])
        );
        assert_ne!(
            hash_of(&from(1), &project, &[]),
            hash_of(&from(1), &one("helpers", HELPERS_V2), &[])
        );
    }

    /// Two helpers import the same name from different modules outside the project: both add
    /// a marker labelled with that name. 0.17.0 ordered those two by a per-process random hash
    /// seed, so the step had two possible hashes and missed its cache at random. The parts are
    /// now sorted by label and text.
    #[test]
    fn same_name_imported_from_two_non_project_modules_hashes_stably() {
        let entry = "from first import load\nfrom second import save\n\ndef my_asset():\n    return save(load())\n";
        let project = sources(&[
            (
                "first",
                "from numpy import array\n\ndef load():\n    return array([1])\n",
            ),
            (
                "second",
                "from jax.numpy import array\n\ndef save(x):\n    return array(x)\n",
            ),
        ]);
        for _ in 0..50 {
            assert_eq!(hash_of(entry, &project, &[]), STABLE_TWO_MARKERS);
        }
    }

    const STABLE_TWO_MARKERS: &str =
        "a1755524889bd9195e25225e96c217f3fede7587f0b64dc6be0d5c17b08e638a";

    #[test]
    fn import_chains_are_followed_six_modules_deep() {
        // pipeline -> m1 -> m2 -> ... : each `mN.f` calls `mN+1.f`.
        let project = |modules: usize, leaf: u32| -> HashMap<String, String> {
            (1..=modules)
                .map(|i| {
                    let src = if i == modules {
                        format!("def f():\n    return {leaf}\n")
                    } else {
                        format!(
                            "from m{} import f as g\n\ndef f():\n    return g()\n",
                            i + 1
                        )
                    };
                    (format!("m{i}"), src)
                })
                .collect()
        };
        let entry = "from m1 import f\n\ndef my_asset():\n    return f()\n";
        assert_ne!(
            hash_of(entry, &project(6, 1), &[]),
            hash_of(entry, &project(6, 2), &[])
        );
        assert_eq!(
            hash_of(entry, &project(7, 1), &[]),
            hash_of(entry, &project(7, 2), &[])
        );
    }

    // ─── Upgrade: what must not move (#194) ──────────────────────────────────────

    const GOLDEN_PIPELINE: &str = r#"
import json
import os.path as osp

import helpers
import pkg.core
import pkg.sub.deep as deep_mod
from barca import asset
from helpers import compute
from numpy import array as arr
from pkg import core, transform
from pkg.sub.deep import deep

RATE = 2
LIMITS = [RATE, 10]
TABLE: dict = {"a": RATE}


def local_helper(x):
    return x * RATE


def chained(x):
    return local_helper(x) + LIMITS[0]


@asset()
def no_deps():
    return {"v": 1}


@asset()
def local_only(x):
    return chained(x) + TABLE["a"]


@asset()
def from_import():
    return compute(1)


@asset()
def module_attr():
    return helpers.compute(2) + helpers.BASE


@asset()
def package_reexport():
    return transform(RATE)


@asset()
def dotted():
    return pkg.core.transform(1) + deep_mod.deep(2) + core.transform(3) + deep(4)


@asset()
def third_party(rows):
    return arr(json.dumps(osp.join("a", str(rows))))


@asset(inputs={"rows": from_import})
def downstream(rows, RATE=None):
    out = [local_helper(r) for r in rows]
    total = sum(out)
    return {"total": total, "f": f"{RATE}", "lam": (lambda v: v + 1)(total)}


@asset()
def shadowing(helpers, json):
    # A parameter named like a module binding: `helpers.compute` is still counted (rule 2).
    return helpers.compute(json)
"#;

    const GOLDEN_MODULES: &[(&str, &str)] = &[
        (
            "helpers",
            "import json\nimport os.path as osp\nfrom util import shared\n\nBASE = 3\n\n\ndef _inner(x):\n    return x + BASE\n\n\ndef compute(x):\n    return json.dumps(_inner(x)) + shared() + osp.sep\n\n\ndef unused():\n    return 0\n",
        ),
        ("util", "def shared():\n    return 's'\n"),
        ("pkg", "from .core import transform\nfrom . import core\n"),
        (
            "pkg.core",
            "from .consts import SCALE\n\n\ndef transform(x):\n    return x * SCALE\n",
        ),
        ("pkg.consts", "SCALE = 2\n"),
        ("pkg.sub", ""),
        (
            "pkg.sub.deep",
            "from ..core import transform\n\n\ndef deep(x):\n    return transform(x) + 1\n",
        ),
    ];

    const GOLDEN_0_17_0: &[(&str, &str)] = &[
        ("no_deps", ""),
        (
            "local_only",
            "a885fa53cee881b1f76c3af8e76638f345eca39337b65dd09a2ab4495b0387b1",
        ),
        (
            "from_import",
            "5514b2c7cdb44f06c0317295acc5976fed60e12c05764b5223970bf7805b5d67",
        ),
        (
            "module_attr",
            "5514b2c7cdb44f06c0317295acc5976fed60e12c05764b5223970bf7805b5d67",
        ),
        (
            "package_reexport",
            "bb08e08483ac3b61f61cf3e64b649c372265d2b7757df694dd7e635df538f39f",
        ),
        (
            "dotted",
            "a5d212ff19f799b07a03f16cd3422eed261ac0965e42cf42d8c37377e25e60a5",
        ),
        (
            "third_party",
            "1265049b33b6c3f0a23c88d7b999974eace45f19342da9240547f9feb66f5dd8",
        ),
        (
            "downstream",
            "4173ad0f780598582304b78134c9d2c831e8e6d75ac511666d1ce59dd44548f3",
        ),
        (
            "shadowing",
            "5514b2c7cdb44f06c0317295acc5976fed60e12c05764b5223970bf7805b5d67",
        ),
    ];

    /// Cone hashes computed by barca 0.17.0 (`main` before #194) for code that uses none of
    /// the patterns #194 started tracking. They must never move: a change here recomputes
    /// every existing cache on upgrade.
    #[test]
    fn cone_hashes_from_0_17_0_are_unchanged() {
        for (function, expected) in GOLDEN_0_17_0 {
            assert_eq!(
                hash_fn(
                    GOLDEN_PIPELINE,
                    function,
                    &sources(GOLDEN_MODULES),
                    &["pkg", "pkg.sub"]
                ),
                *expected,
                "the cone hash of `{function}` changed"
            );
        }
    }
}
