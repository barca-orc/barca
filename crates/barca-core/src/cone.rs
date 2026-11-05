//! Dependency cone: the helper code a step's function reaches, hashed.
//!
//! The cone is computed from source text alone (ruff's AST; nothing is imported), by these
//! rules. "Project module" means a module the [`ModuleSource`] resolves to a file of the
//! project; the standard library and installed packages are never project modules, are never
//! read, and add at most a constant marker.
//!
//! 1. **Definitions.** A module's definitions are its top-level `def`s, `class`es, assignments
//!    and imports ([`ModuleDef`]). A function contributes its defaults and body; a class
//!    contributes its bases, keywords and whole body (methods and class-level code).
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
//! 7. **Nodes.** For a function decorated with `@asset`, `@sensor` or `@task`, the names in the
//!    decorator parts that count towards its definition hash ([`crate::definition::RULES`]) are
//!    uses too, and follow rules 2 to 5: `serializer=FMT`, `@retry(times=N)`. Names in parts
//!    that do not count (`description=TEXT`, the keys in `partitions(REGIONS)`) are not uses, and
//!    neither is a reference to an upstream node (`inputs={"x": up}`). Where a node is reached
//!    as a definition (rule 2), it contributes its definition text, not its decorators as
//!    written.
//!
//! 8. **Cutoff assets.** An `@asset(cutoff=True)` reached by name (a consumer's parameter named
//!    after it, rule 2) adds a constant marker, not its definition or what it uses: what reads it
//!    folds in the bytes it wrote instead of its run hash (`crate::cache::is_cutoff`), so its
//!    code must not reach its consumers another way (#628). Pipelines without `cutoff=True`
//!    hash as before.
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
    star_exports: OnceCell<Option<Vec<String>>>,
    imported_stars: OnceCell<Vec<String>>,
    /// Where the source stops parsing, if it does; known once it has been parsed.
    syntax_error: OnceCell<Option<crate::parse::SyntaxProblem>>,
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
            star_exports: OnceCell::new(),
            imported_stars: OnceCell::new(),
            syntax_error: OnceCell::new(),
        }
    }

    /// Literal import targets for project-resolution validation. Reuse the same
    /// relative import binding rules as cone hashing, including nested imports.
    pub(crate) fn import_modules(&self) -> BTreeSet<String> {
        struct Imports<'p> {
            package: Option<&'p str>,
            names: BTreeSet<String>,
        }
        impl<'a> Visitor<'a> for Imports<'_> {
            fn visit_stmt(&mut self, stmt: &'a Stmt) {
                for (_, binding) in import_bindings(stmt, self.package) {
                    match binding {
                        ModuleDef::ModuleImport { module } => {
                            self.names.insert(module);
                        }
                        ModuleDef::FromImport { module, name } => {
                            if name != "*" {
                                self.names.insert(format!("{module}.{name}"));
                            }
                            self.names.insert(module);
                        }
                        _ => {}
                    }
                }
                // `import pkg.child` also imports the child when no alias is
                // supplied, although the local binding is just `pkg`.
                if let Stmt::Import(import) = stmt {
                    self.names
                        .extend(import.names.iter().map(|alias| alias.name.to_string()));
                }
                visitor::walk_stmt(self, stmt);
            }
        }
        let mut imports = Imports {
            package: self.package.as_deref(),
            names: BTreeSet::new(),
        };
        if self.syntax_error.get().is_some_and(Option::is_some) {
            return imports.names;
        }
        match parse_module(&self.source) {
            Ok(parsed) => {
                let _ = self.syntax_error.set(None);
                visitor::walk_body(&mut imports, &parsed.syntax().body);
            }
            Err(e) => {
                let _ = self
                    .syntax_error
                    .set(Some(crate::parse::SyntaxProblem::of(&self.source, &e)));
            }
        }
        imports.names
    }

    /// Where the module's source stops parsing; `None` when it parses. Parsed at most once,
    /// shared with [`Module::import_modules`].
    pub(crate) fn syntax_error(&self) -> Option<&crate::parse::SyntaxProblem> {
        self.syntax_error
            .get_or_init(|| {
                parse_module(&self.source)
                    .err()
                    .map(|e| crate::parse::SyntaxProblem::of(&self.source, &e))
            })
            .as_ref()
    }

    /// A finite literal export set, parsed once per imported project module.
    pub(crate) fn star_exports(&self) -> Option<Vec<String>> {
        self.star_exports
            .get_or_init(|| crate::decorator_args::literal_star_exports(&self.source))
            .clone()
    }

    /// Resolve a star import using the same package rules as dependency hashing.
    pub(crate) fn star_import_module(
        &self,
        import: &ruff_python_ast::StmtImportFrom,
    ) -> Option<String> {
        if import.level == 0 {
            return import.module.as_ref().map(|m| m.to_string());
        }
        let base = package_ancestor(
            self.package.as_deref()?,
            (import.level as usize).saturating_sub(1),
        )?;
        Some(
            import
                .module
                .as_ref()
                .map_or(base.clone(), |m| format!("{base}.{m}")),
        )
    }

    /// Every literal star-import target, including module-level control-flow blocks.
    fn imported_stars(&self) -> &[String] {
        self.imported_stars.get_or_init(|| {
            struct Stars<'p> {
                package: Option<&'p str>,
                modules: BTreeSet<String>,
            }
            impl<'a> Visitor<'a> for Stars<'_> {
                fn visit_stmt(&mut self, stmt: &'a Stmt) {
                    for (_, binding) in import_bindings(stmt, self.package) {
                        if let ModuleDef::FromImport { module, name } = binding
                            && name == "*"
                            && module != "barca"
                        {
                            self.modules.insert(module);
                        }
                    }
                    visitor::walk_stmt(self, stmt);
                }
            }
            let mut stars = Stars {
                package: self.package.as_deref(),
                modules: BTreeSet::new(),
            };
            if let Ok(parsed) = parse_module(&self.source) {
                visitor::walk_body(&mut stars, &parsed.syntax().body);
            }
            stars.modules.into_iter().collect()
        })
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

    /// Other project files a worker may load under `name` instead of [`ModuleSource::module`],
    /// when which one it loads cannot be known while planning. Each is a [`Module`] under a
    /// name of its own; its definitions are added to the cone beside `module`'s.
    fn alternatives(&self, _name: &str) -> Vec<Rc<Module>> {
        Vec::new()
    }

    /// The project-relative path of the file `name` is read from (`pipelines/common.py`), for
    /// labels people read. `None` when it is not known; the dotted name is shown instead.
    fn file_label(&self, _name: &str) -> Option<String> {
        None
    }
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
    /// An `@asset(cutoff=True)` (rule 8): reached by name, it adds [`CUTOFF_MARKER`] and
    /// nothing it uses.
    cutoff: bool,
}

/// What an `@asset(cutoff=True)` contributes where it is reached by name (rule 8).
const CUTOFF_MARKER: &str = "<an @asset(cutoff=True): what reads it depends on its bytes>";

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
    collect(pipeline, function_name, modules).map_or_else(String::new, Cone::hash)
}

/// [`cone_hash`], and a short digest of each definition in the cone by label: `name` for a
/// definition of the pipeline file itself, `path/to/module.py:name` for one of a project module
/// ([`ModuleSource::file_label`]; `module.name:name` when the module has no file label), and
/// `path/to/module.py` for a module added whole (rule 5; `module.name:*` without a file label). Recorded with each materialization so
/// `barca status` can say which helper changed (#558); they are not part of any hash.
pub fn cone_hash_and_parts(
    pipeline: &Module,
    function_name: &str,
    modules: &dyn ModuleSource,
) -> (String, Vec<(String, String)>) {
    let (hash, parts, _) = cone_hash_parts_and_sources(pipeline, function_name, modules);
    (hash, parts)
}

/// [`cone_hash_and_parts`], and the source of every definition in the cone (for static checks
/// over the code a step reaches, such as [`crate::env_reads`]; not part of any hash).
pub fn cone_hash_parts_and_sources(
    pipeline: &Module,
    function_name: &str,
    modules: &dyn ModuleSource,
) -> (String, Vec<(String, String)>, Vec<Rc<str>>) {
    let Some(cone) = collect(pipeline, function_name, modules) else {
        return (String::new(), Vec::new(), Vec::new());
    };
    let parts = cone.digests();
    let sources = cone.parts.iter().map(|(_, text)| text.clone()).collect();
    (cone.hash(), parts, sources)
}

fn collect<'a>(
    pipeline: &Module,
    function_name: &str,
    modules: &'a dyn ModuleSource,
) -> Option<Cone<'a>> {
    let Some(ModuleDef::Function(code)) = pipeline.definitions().get(function_name) else {
        return None;
    };
    let mut cone = Cone {
        modules,
        visited: HashSet::new(),
        parts: Vec::new(),
    };
    let site = Site {
        module: pipeline,
        is_pipeline: true,
    };
    cone.follow(site, &code.uses);
    Some(cone)
}

/// The module whose definitions a use is resolved against.
#[derive(Clone, Copy)]
struct Site<'m> {
    module: &'m Module,
    /// The pipeline file's own definitions are labelled by bare name, an imported module's by
    /// `module:name`.
    is_pipeline: bool,
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
    /// A short digest of each part by its label, sorted; parts that share a label (a
    /// definition added twice) share one entry.
    fn digests(&self) -> Vec<(String, String)> {
        let mut by_label: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for (label, text) in &self.parts {
            let shown = match label.split_once(':') {
                Some((module, name)) => match self.modules.file_label(module) {
                    Some(file) if name == "*" => file,
                    Some(file) => format!("{file}:{name}"),
                    None => label.clone(),
                },
                None => label.clone(),
            };
            by_label
                .entry(shown)
                .or_default()
                .push(crate::hash::short_digest(text.as_bytes()));
        }
        by_label
            .into_iter()
            .map(|(label, mut digests)| {
                digests.sort();
                (label, digests.join(","))
            })
            .collect()
    }

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
                    ModuleDef::Function(code) if code.cutoff => {
                        self.parts.push((site.label(name), CUTOFF_MARKER.into()));
                    }
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
                    } => self.import(module, original, name),
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
            self.attribute(&module, &attr);
        }

        for reference in &uses.values {
            if let Some((binding, attrs)) = binding_of(definitions, reference)
                && let Some(ModuleUse::Whole(module)) = self.module_use(binding, &attrs)
            {
                self.whole_module(&module);
            }
        }

        for local in &uses.local {
            match self.module_use(&local.binding, &local.attrs) {
                Some(ModuleUse::Attribute(module, attr)) => self.attribute(&module, &attr),
                Some(ModuleUse::Whole(module)) => self.whole_module(&module),
                None => {
                    if let ModuleDef::FromImport { module, name } = &local.binding
                        && self.modules.module(module).is_some()
                        && self.visited.insert(format!("{module}:{name}"))
                    {
                        self.import(module, name, &local.bound);
                    }
                }
            }
        }
    }

    /// What `binding.attrs...` uses, when `binding` is bound to a project module. `None` for
    /// anything else: a name that is not a module, or a module outside the project
    /// (`json.dumps`).
    fn module_use<S: AsRef<str>>(&self, binding: &ModuleDef, attrs: &[S]) -> Option<ModuleUse> {
        let base = match binding {
            ModuleDef::ModuleImport { module } => module.clone(),
            // `from pkg import mod`, where `pkg.mod` is itself a module, or `from wrapper
            // import helpers`, where `wrapper` binds `helpers` to a module by its own import.
            ModuleDef::FromImport { module, name } => self.module_named(module, name)?,
            _ => return None,
        };
        self.module_chain(base, attrs)
    }

    /// What `base.attrs...` uses, `base` being a project module.
    fn module_chain<S: AsRef<str>>(&self, base: String, attrs: &[S]) -> Option<ModuleUse> {
        let is_module = |name: &str| self.modules.module(name).is_some();
        // The whole chain names a module (`helpers`, `pkg.mod`): it is used as a value.
        let whole = join_dotted(&base, attrs);
        if is_module(&whole) {
            return Some(ModuleUse::Whole(whole));
        }
        // Otherwise the longest prefix that is a module; the next segment is the attribute read
        // from it (`pkg.mod.f.x` reads `f` from `pkg.mod`), unless that module binds the
        // attribute to another module by an import (`wrapper.helpers.f`, where `wrapper.py`
        // does `import helpers`): the rest of the chain is then read from that module.
        let k = (0..attrs.len())
            .rev()
            .find(|&k| is_module(&join_dotted(&base, &attrs[..k])))?;
        let module = join_dotted(&base, &attrs[..k]);
        let attr = attrs[k].as_ref();
        match self.reexported_module(&module, attr) {
            Some(next) => self.module_chain(next, &attrs[k + 1..]),
            None => Some(ModuleUse::Attribute(module, attr.to_string())),
        }
    }

    /// The project module `from module import name` binds: the submodule `module.name`, or a
    /// module `module` itself binds to `name` by an import ([`Self::reexported_module`]).
    fn module_named(&self, module: &str, name: &str) -> Option<String> {
        let full = join_dotted(module, &[name]);
        if self.modules.module(&full).is_some() {
            return Some(full);
        }
        self.reexported_module(module, name)
    }

    /// The module that project module `module` binds to `name` by an import of its own:
    /// `import helpers [as name]`, or `from other import name` that itself names one, followed
    /// through any number of re-exports. Cycles end in `None`.
    fn reexported_module(&self, module: &str, name: &str) -> Option<String> {
        // The re-exports already followed; most lookups end before the first one.
        let mut seen: Vec<(String, String)> = Vec::new();
        let (mut module, mut name) = (module.to_string(), name.to_string());
        loop {
            match self.modules.module(&module)?.definitions().get(&name)? {
                // Not necessarily a project module itself: `import pkg.mod` binds `pkg`, which
                // may be a namespace package; [`Self::module_chain`] finds `pkg.mod` from it.
                ModuleDef::ModuleImport { module: target } => return Some(target.clone()),
                ModuleDef::FromImport {
                    module: next,
                    name: original,
                } => {
                    let full = join_dotted(next, &[original]);
                    if self.modules.module(&full).is_some() {
                        return Some(full);
                    }
                    if seen.iter().any(|(m, n)| m == next && n == original) {
                        return None;
                    }
                    seen.push((module, name));
                    (module, name) = (next.clone(), original.clone());
                }
                _ => return None,
            }
        }
    }

    /// Rule 4: `module.attr`, added once however many times it is read.
    fn attribute(&mut self, module: &str, attr: &str) {
        if self.visited.insert(format!("{module}:{attr}")) {
            self.import(module, attr, attr);
        }
    }

    /// Rule 3: `module`'s definition of `name` and what it uses, following re-exports.
    /// `bound` is the name the importing code bound it to.
    ///
    /// Deliberately not deduplicated through `visited`: a definition imported here and also
    /// reached from inside its module (a recursive helper, say) is added twice. Every release
    /// since 0.10 has hashed it that way; adding it once would change those hashes and
    /// recompute their caches (`recursive_helper_hash_from_0_17_0_is_unchanged`).
    ///
    /// Import chains of any length are followed. What ends them is that every definition's
    /// uses are followed once (`visited`), and a re-export chain that comes back to a
    /// `(module, name)` it already passed (`chain`) adds only its marker.
    fn import(&mut self, module: &str, name: &str, bound: &str) {
        self.import_along(module, name, bound, &mut Vec::new());
    }

    /// [`Self::import`], `chain` being the `(module, name)` re-exports already followed to
    /// get here.
    fn import_along(
        &mut self,
        module: &str,
        name: &str,
        bound: &str,
        chain: &mut Vec<(String, String)>,
    ) {
        if chain.iter().any(|(m, n)| m == module && n == name) {
            // A re-export cycle (`a` imports `x` from `b`, `b` imports it from `a`): Python
            // cannot import it either. The marker records the binding.
            self.parts
                .push((bound.to_string(), format!("import:{module}:{bound}").into()));
            return;
        }
        chain.push((module.to_string(), name.to_string()));
        let found = self.modules.module(module);
        if !found.is_some_and(|imported| self.definition(&imported, name, chain)) {
            // Not a project module, or nothing barca can read `name` from there (defined
            // dynamically, a module binding, a star import): a constant marker for the binding,
            // so the hash still says the name is imported.
            self.parts
                .push((bound.to_string(), format!("import:{module}:{bound}").into()));
        }
        for alternative in self.modules.alternatives(module) {
            self.definition(&alternative, name, chain);
        }
        chain.pop();
    }

    /// Add `module`'s definition of `name` and what it uses. `false` when it has none that can
    /// be read statically.
    fn definition(
        &mut self,
        module: &Module,
        name: &str,
        chain: &mut Vec<(String, String)>,
    ) -> bool {
        match module.definitions().get(name) {
            Some(ModuleDef::Function(code)) if code.cutoff => {
                self.parts
                    .push((format!("{}:{name}", module.name), CUTOFF_MARKER.into()));
                true
            }
            Some(
                ModuleDef::Function(code) | ModuleDef::Class(code) | ModuleDef::Assignment(code),
            ) => {
                self.parts
                    .push((format!("{}:{name}", module.name), code.source_text.clone()));
                let site = Site {
                    module,
                    is_pipeline: false,
                };
                self.follow(site, &code.uses);
                true
            }
            Some(ModuleDef::FromImport {
                module: next,
                name: original,
            }) => {
                self.import_along(next, original, name, chain);
                true
            }
            None if !module.imported_stars().is_empty() => {
                // Star exports need not be literal assignments in this module. Preserve all
                // contributing sources rather than guessing which export installed a name.
                self.whole(module);
                true
            }
            Some(ModuleDef::ModuleImport { .. }) | None => false,
        }
    }

    /// Rule 5: the module's whole source, and everything its definitions use.
    fn whole_module(&mut self, name: &str) {
        let candidates = self.modules.module(name).into_iter();
        for module in candidates.chain(self.modules.alternatives(name)) {
            self.whole(&module);
        }
    }

    fn whole(&mut self, module: &Module) {
        if !self.visited.insert(format!("{}:*", module.name)) {
            return;
        }
        self.parts
            .push((format!("{}:*", module.name), module.source.clone()));
        let every_definition = Uses {
            names: module.definitions().keys().cloned().collect(),
            local: module
                .imported_stars()
                .iter()
                .map(|target| LocalUse {
                    bound: "*".into(),
                    binding: ModuleDef::ModuleImport {
                        module: target.clone(),
                    },
                    attrs: Vec::new(),
                })
                .collect(),
            ..Uses::default()
        };
        self.follow(
            Site {
                module,
                is_pipeline: false,
            },
            &every_definition,
        );
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
        cutoff: false,
    };
    let mut defs: HashMap<String, ModuleDef> = HashMap::new();
    let barca = crate::decorator_args::BarcaNames::of(&parsed.syntax().body);
    let mut conservative_functions = Vec::new();
    struct StaticImports<'p> {
        package: Option<&'p str>,
        uses: Vec<LocalUse>,
    }
    impl<'a> Visitor<'a> for StaticImports<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            self.uses.extend(
                import_bindings(stmt, self.package)
                    .into_iter()
                    .filter(|(_, binding)| !is_group_import(binding))
                    .map(|(bound, binding)| {
                        // A star import can install any decorator/helper from this module.
                        let binding = match binding {
                            ModuleDef::FromImport { module, name } if name == "*" => {
                                ModuleDef::ModuleImport { module }
                            }
                            other => other,
                        };
                        LocalUse {
                            bound,
                            binding,
                            attrs: Vec::new(),
                        }
                    }),
            );
            visitor::walk_stmt(self, stmt);
        }
    }
    let mut static_imports = StaticImports {
        package,
        uses: Vec::new(),
    };
    visitor::walk_body(&mut static_imports, &parsed.syntax().body);
    let has_foreign_star = static_imports.uses.iter().any(|import| {
        import.bound == "*"
            && matches!(&import.binding, ModuleDef::ModuleImport { module } if module != "barca")
    });
    let conditional_module = OnceCell::new();
    let conservative_code = || {
        conditional_module.get_or_init(|| {
            let mut uses = UseCollector::new(package);
            uses.stmts(&parsed.syntax().body);
            uses.uses.local.extend(static_imports.uses.iter().cloned());
            Code {
                source_text: source.into(),
                uses: uses.uses,
                cutoff: false,
            }
        })
    };
    let mut conditional_names = HashSet::new();

    for stmt in &parsed.syntax().body {
        match stmt {
            Stmt::FunctionDef(func) => {
                let mut uses = UseCollector::new(package);
                uses.function(func);
                // A node (`@asset`, `@sensor`, `@task`) contributes what its definition hash
                // covers, wherever it is reached from: the decorator parts that count, in
                // canonical form, and the names in them (rule 7). Any other function
                // contributes its text as written, decorators included.
                let function = match crate::definition::node_definition(func, source, &barca) {
                    Some(definition) => {
                        uses.exprs(definition.followed.iter().copied());
                        if definition.conservative_module {
                            conservative_functions.push(func.name.to_string());
                            uses.stmts(&parsed.syntax().body);
                        }
                        Code {
                            source_text: definition.text.into(),
                            uses: uses.uses,
                            cutoff: crate::parse::is_cutoff_asset(func, &barca),
                        }
                    }
                    None => {
                        uses.exprs(func.decorator_list.iter().map(|d| &d.expression));
                        code(func, uses)
                    }
                };
                // Project-aware discovery can recognize nodes after a safe star import.
                // The standalone cone has no export resolver: keep decorated functions
                // conservative and follow the whole imported module, including constants
                // installed under names that never appear in explicit import bindings.
                if has_foreign_star && !func.decorator_list.is_empty() {
                    conservative_functions.push(func.name.to_string());
                }
                defs.insert(func.name.to_string(), ModuleDef::Function(function));
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
                if assign
                    .targets
                    .iter()
                    .any(|target| !matches!(target, Expr::Name(_)))
                {
                    let mut binder = Binder::new(package);
                    for target in &assign.targets {
                        binder.visit_expr(target);
                    }
                    for name in binder.finish().bindings.keys() {
                        conditional_names.insert(name.clone());
                        defs.insert(
                            name.clone(),
                            ModuleDef::Assignment(conservative_code().clone()),
                        );
                    }
                    continue;
                }
                // Organizational declarations must not become dependencies when a local
                // variable happens to share their name (the cone is conservative about locals).
                if let Expr::Call(call) = assign.value.as_ref()
                    && barca.resolve(&call.func) == Some("group")
                {
                    continue;
                }
                for target in &assign.targets {
                    if let Expr::Name(n) = target {
                        let mut uses = UseCollector::new(package);
                        uses.expr(&assign.value);
                        defs.insert(n.id.to_string(), ModuleDef::Assignment(code(assign, uses)));
                    }
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Some(Expr::Call(call)) = assign.value.as_deref()
                    && barca.resolve(&call.func) == Some("group")
                {
                    continue;
                }
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
            Stmt::ImportFrom(_) => defs.extend(
                import_bindings(stmt, package)
                    .into_iter()
                    .filter(|(_, binding)| !is_group_import(binding)),
            ),
            Stmt::If(_)
            | Stmt::Try(_)
            | Stmt::For(_)
            | Stmt::While(_)
            | Stmt::With(_)
            | Stmt::AugAssign(_)
            | Stmt::Match(_) => {
                // Do not select a conditional binding by executing Python or by guessing
                // which branch runs. The module may retain an earlier binding, install a
                // different import or define a fallback function. Represent each possible
                // bound name using the existing conservative Code path: the module's source
                // plus its references and all statically visible imports. Ordinary names
                // that do not reach these bindings retain their selective cones.
                let mut binder = Binder::new(package);
                binder.visit_stmt(stmt);
                let names = binder.finish().bindings;
                if !names.is_empty() {
                    let conservative = conservative_code();
                    for name in names.keys().filter(|name| name.as_str() != "*") {
                        conditional_names.insert(name.clone());
                        defs.insert(name.clone(), ModuleDef::Assignment(conservative.clone()));
                    }
                }
            }
            _ => {}
        }
    }
    // An assignment can consume the previous value (`f = wrap(f)`), or an earlier alias
    // can capture it before any later function/class/import replaces `f`. The final binding
    // alone cannot describe that provenance. Keep it for every conditional name, even when
    // the final replacement would otherwise be independently resolvable.
    for name in conditional_names {
        if let Some(definition) = defs.get_mut(&name) {
            let conservative = conditional_module
                .get()
                .expect("a conditional binding was collected");
            match definition {
                ModuleDef::Function(code) | ModuleDef::Class(code) => {
                    *code = conservative.clone();
                    // Preserve an entry function's kind and make its cone reach the source
                    // too: cone_hash initially follows uses without adding the entry text.
                    code.uses.names.insert(name);
                }
                _ => *definition = ModuleDef::Assignment(conservative.clone()),
            }
        }
    }
    // Unproven node bindings may have changed in defaults, conditionals or global writes.
    // Include all statically collected module definitions and imports; resolving just the
    // original import could miss the implementation actually installed at evaluation time.
    let names: Vec<_> = defs.keys().cloned().collect();
    for name in conservative_functions {
        if let Some(ModuleDef::Function(code)) = defs.get_mut(&name) {
            code.uses.names.extend(names.iter().cloned());
            code.uses.values.extend(names.iter().cloned());
            code.uses.local.extend(static_imports.uses.iter().cloned());
        }
    }
    if has_foreign_star {
        for definition in defs.values_mut() {
            if let ModuleDef::Function(code)
            | ModuleDef::Class(code)
            | ModuleDef::Assignment(code) = definition
            {
                code.uses.local.extend(static_imports.uses.iter().cloned());
            }
        }
    }
    defs
}

/// The group factory is metadata too: an unrelated local variable named `group`
/// must not begin following this import when a hierarchy is added to the file.
fn is_group_import(binding: &ModuleDef) -> bool {
    matches!(binding, ModuleDef::FromImport { module, name } if module == "barca" && name == "group")
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

    /// Defaults execute in the enclosing scope, before the function's parameters bind.
    /// Decorators and annotations retain their existing policy.
    fn defaults(&mut self, parameters: &ruff_python_ast::Parameters) {
        for parameter in parameters.iter() {
            self.exprs(parameter.default());
        }
    }

    fn function(&mut self, func: &ruff_python_ast::StmtFunctionDef) {
        self.defaults(&func.parameters);
        let scope = self.block_scope(Some(&func.parameters), &func.body);
        self.scoped(scope, |c| c.stmts(&func.body));
    }

    /// A class contributes its decorators, bases and body.
    fn class(&mut self, class: &ruff_python_ast::StmtClassDef) {
        self.exprs(class.decorator_list.iter().map(|d| &d.expression));
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
                c.target(&generator.target);
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

    /// Assignment evaluates receivers and indices, but does not read a bare binding name.
    fn target(&mut self, target: &Expr) {
        match target {
            Expr::Attribute(a) => self.expr(&a.value),
            Expr::Subscript(s) => {
                self.expr(&s.value);
                self.expr(&s.slice);
            }
            Expr::Tuple(t) => {
                for item in &t.elts {
                    self.target(item);
                }
            }
            Expr::List(l) => {
                for item in &l.elts {
                    self.target(item);
                }
            }
            Expr::Starred(s) => self.target(&s.value),
            _ => {}
        }
    }

    /// Patterns contain evaluated value/class/key expressions as well as capture names.
    /// Let Ruff walk nested patterns; only expressions contribute references.
    fn pattern(&mut self, pattern: &Pattern) {
        struct Values<'c, 'p>(&'c mut UseCollector<'p>);
        impl<'a> Visitor<'a> for Values<'_, '_> {
            fn visit_expr(&mut self, expr: &'a Expr) {
                self.0.expr(expr);
            }
        }
        Values(self).visit_pattern(pattern);
    }

    // Extend this traversal one construct family at a time: following previously omitted
    // references invalidates existing cached code. Keep the manual and regression controls
    // aligned with each deliberate extension.
    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Expr(s) => self.expr(&s.value),
            Stmt::Return(s) => self.exprs(s.value.as_deref()),
            Stmt::Assign(s) => {
                self.expr(&s.value);
                for target in &s.targets {
                    self.target(target);
                }
            }
            Stmt::AugAssign(s) => {
                self.expr(&s.target);
                self.expr(&s.value);
            }
            Stmt::AnnAssign(s) => {
                self.exprs(s.value.as_deref());
                self.target(&s.target);
            }
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
                self.target(&s.target);
                self.stmts(&s.body);
                self.stmts(&s.orelse);
            }
            Stmt::While(s) => {
                self.expr(&s.test);
                self.stmts(&s.body);
                self.stmts(&s.orelse);
            }
            Stmt::With(s) => {
                for item in &s.items {
                    self.expr(&item.context_expr);
                    if let Some(target) = &item.optional_vars {
                        self.target(target);
                    }
                }
                self.stmts(&s.body);
            }
            Stmt::FunctionDef(s) => {
                self.exprs(s.decorator_list.iter().map(|d| &d.expression));
                self.function(s);
            }
            Stmt::ClassDef(s) => self.class(s),
            Stmt::Try(s) => {
                self.stmts(&s.body);
                for handler in &s.handlers {
                    let ExceptHandler::ExceptHandler(handler) = handler;
                    self.exprs(handler.type_.as_deref());
                    self.stmts(&handler.body);
                }
                self.stmts(&s.orelse);
                self.stmts(&s.finalbody);
            }
            Stmt::Match(s) => {
                self.expr(&s.subject);
                for case in &s.cases {
                    self.pattern(&case.pattern);
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
            Expr::Set(s) => self.exprs(&s.elts),
            Expr::Tuple(t) => self.exprs(&t.elts),
            Expr::Lambda(l) => {
                if let Some(parameters) = &l.parameters {
                    self.defaults(parameters);
                }
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
mod tests;
