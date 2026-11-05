//! Which project file a Python module name refers to, for the dependency cone
//! ([`crate::cone`]).
//!
//! The answer has to be the file the worker's `import` loads, or the hash would track a file
//! Python never runs. So this mirrors `python/barca/_worker.py::load_module` and
//! `python/barca/_source_import.py`:
//!
//! - A pipeline file **inside a package** (every directory from the project root down to it has
//!   an `__init__.py`) is imported by its dotted name with only the **root** on the import
//!   path: `import helpers` is `<root>/helpers.py`, a sibling is `from .helpers import f` or
//!   `from pkg.helpers import f`.
//! - Other pipeline files use ordinary root/namespace module identities when possible,
//!   with **their own directory, then the root** on the import path. A sibling shadows a
//!   root module. Previous tasks' directories never remain implicit import paths.
//! - Actual conflicting project imports are refused before execution. Qualified names
//!   identify their source; ordinary installed/stdlib imports stay external.
//!
//! Within one path entry Python's own order applies: a package (`name/__init__.py`) before a
//! module (`name.py`), and a directory without `__init__.py` is a namespace package only when
//! no entry has a regular package or module of that name.
//!
//! Nothing is walked. A name is resolved the first time the cone asks for it, by probing the
//! few paths it could be; a file is read at most once per plan ([`SourceFiles`]), and a module
//! nobody imports is never opened. The directories searched are the path entries above and the
//! packages under them, so the standard library, installed packages (wherever the virtualenv
//! sits) and anything outside those entries are never read.

use crate::cone::{self, Module, ModuleSource};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The source of every project file read while planning, by path: each is read once, however
/// many pipelines import it.
#[derive(Default)]
struct SourceFiles {
    sources: RefCell<HashMap<PathBuf, Option<Rc<str>>>>,
    /// Modules by file and import name, so a helper that several import paths reach under the
    /// same name is parsed once per plan.
    modules: RefCell<HashMap<(PathBuf, String), Rc<Module>>>,
    /// Files actually read from disk, in order.
    #[cfg(test)]
    read: RefCell<Vec<PathBuf>>,
}

impl SourceFiles {
    /// Record a file that is already in memory (a pipeline file), so importing it does not read
    /// it again.
    fn insert(&self, path: PathBuf, source: Rc<str>) {
        self.sources.borrow_mut().insert(path, Some(source));
    }

    /// The file's text; `None` when it does not exist or is not readable UTF-8.
    fn get(&self, path: &Path) -> Option<Rc<str>> {
        if let Some(known) = self.sources.borrow().get(path) {
            return known.clone();
        }
        let source: Option<Rc<str>> = std::fs::read_to_string(path).ok().map(Rc::from);
        #[cfg(test)]
        if source.is_some() {
            self.read.borrow_mut().push(path.to_path_buf());
        }
        self.sources
            .borrow_mut()
            .insert(path.to_path_buf(), source.clone());
        source
    }

    /// The module in the file at `path`, imported as `name`; `None` when there is no such file.
    fn module(&self, path: PathBuf, name: &str, is_package: bool) -> Option<Rc<Module>> {
        let source = self.get(&path)?;
        let module = self
            .modules
            .borrow_mut()
            .entry((path, name.to_string()))
            .or_insert_with(|| Rc::new(Module::new(name, source, is_package)))
            .clone();
        Some(module)
    }
}

/// How the worker imports one pipeline file.
#[derive(Debug, PartialEq)]
pub struct PipelineLayout {
    /// The name the file is imported under: `pipelines.reconcile` inside a package, otherwise
    /// a name with no package (the file is loaded by path).
    pub module: String,
    /// The file is a package's `__init__.py`.
    pub is_package: bool,
    /// The directories its absolute imports are searched in, in order.
    pub path: Vec<PathBuf>,
}

impl PipelineLayout {
    /// `file` and `root` (the project root, the worker's working directory) must both be
    /// canonical.
    pub fn of(file: &Path, root: &Path) -> Self {
        let dir = file.parent().unwrap_or(root);
        Self::in_dir(file, root, package_of(dir, root).as_deref())
    }

    /// As [`PipelineLayout::of`], given what [`package_of`] says about the file's directory.
    /// Fully packaged files search the root; namespace/loose files keep sibling-first
    /// imports while sharing their ordinary root-relative module identity.
    fn in_dir(file: &Path, root: &Path, dir_package: Option<&str>) -> Self {
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let is_init = stem == "__init__";
        let module = match dir_package {
            Some(package) if !is_init => Some(format!("{package}.{stem}")),
            Some(package) => Some(package.to_string()),
            _ => None,
        };
        if let Some(module) = module {
            return PipelineLayout {
                module,
                is_package: is_init,
                path: vec![root.to_path_buf()],
            };
        }
        let dir = file.parent().unwrap_or(root).to_path_buf();
        PipelineLayout {
            module: if file.strip_prefix(root).is_ok() {
                qualified_path(file, root)
            } else {
                stem.replace('.', "_")
            },
            is_package: is_init,
            path: if dir == root {
                vec![dir]
            } else {
                vec![dir, root.to_path_buf()]
            },
        }
    }
}

/// The dotted package name of `dir` when it is a package under the root: every directory from
/// the root down to it has an `__init__.py`. `None` for the root itself, a directory outside
/// it, or one at or below a directory without `__init__.py`.
fn package_of(dir: &Path, root: &Path) -> Option<String> {
    let relative = dir.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = Vec::new();
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if !current.join("__init__.py").is_file() {
            return None;
        }
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// A pipeline file of the plan. Stems are used only to diagnose explicit node inputs
/// that would otherwise rely on an off-path fallback.
struct PipelineFile {
    /// Canonical, like everything this module compares paths with.
    path: PathBuf,
    dir: PathBuf,
    stem: String,
    source: Rc<str>,
}

/// What a dotted name is, on one import path.
#[derive(Clone)]
enum Found {
    /// A module, and for a package the directory its submodules are searched in.
    Module(Rc<Module>, Option<PathBuf>, PathBuf),
    /// A namespace package: directories without `__init__.py`, one per path entry.
    Namespace(Vec<PathBuf>),
    Missing,
}

/// The modules importable from the pipeline files that share one import path.
struct ImportPath {
    /// The directory of the pipeline files importing through this path.
    dir: PathBuf,
    path: Vec<PathBuf>,
    /// The project root, for labels that do not depend on where the project lives.
    root: PathBuf,
    /// The plan's pipeline files by stem, each list ordered by directory.
    by_stem: Rc<HashMap<String, Vec<Rc<PipelineFile>>>>,
    files: Rc<SourceFiles>,
    found: RefCell<HashMap<String, Found>>,
}

impl ImportPath {
    /// What `name` is on the import path, by Python's rules; never a pipeline file reached by
    /// its stem (see [`ImportPath::module`]). Used for `name` itself and as the parent of
    /// `name.sub`.
    fn find(&self, name: &str) -> Found {
        if let Some(found) = self.found.borrow().get(name) {
            return found.clone();
        }
        let found = match name.rsplit_once('.') {
            None => self.search(name, name, &self.path),
            Some((parent, leaf)) => match self.find(parent) {
                Found::Module(_, Some(dir), _) => self.search(name, leaf, &[dir]),
                Found::Namespace(dirs) => self.search(name, leaf, &dirs),
                _ => Found::Missing,
            },
        };
        self.found
            .borrow_mut()
            .insert(name.to_string(), found.clone());
        found
    }

    /// Python's path search for `leaf` (the last segment of `name`) in `dirs`.
    fn search(&self, name: &str, leaf: &str, dirs: &[PathBuf]) -> Found {
        if leaf.is_empty() {
            return Found::Missing;
        }
        let mut portions = Vec::new();
        for dir in dirs {
            let package_dir = dir.join(leaf);
            if let Some(module) = self
                .files
                .module(package_dir.join("__init__.py"), name, true)
            {
                return Found::Module(
                    module,
                    Some(package_dir.clone()),
                    package_dir.join("__init__.py"),
                );
            }
            if let Some(module) = self
                .files
                .module(dir.join(format!("{leaf}.py")), name, false)
            {
                return Found::Module(module, None, dir.join(format!("{leaf}.py")));
            }
            if package_dir.is_dir() {
                portions.push(package_dir);
            }
        }
        if portions.is_empty() {
            Found::Missing
        } else {
            Found::Namespace(portions)
        }
    }

    /// Pipeline files in other directories whose stem is the top-level name `name`.
    fn pipelines_named(&self, name: &str) -> impl Iterator<Item = &Rc<PipelineFile>> {
        let named = self.by_stem.get(name).map_or(&[][..], Vec::as_slice);
        named.iter().filter(|p| p.dir != self.dir)
    }
}

/// Resolve only the ordinary Python import path. Another pipeline's directory
/// never becomes an implicit source merely because that worker ran it earlier.
impl ModuleSource for ImportPath {
    fn module(&self, name: &str) -> Option<Rc<Module>> {
        match self.find(name) {
            Found::Module(module, _, _) => Some(module),
            Found::Namespace(_) | Found::Missing => None,
        }
    }

    fn file_label(&self, name: &str) -> Option<String> {
        let Found::Module(_, _, path) = self.find(name) else {
            return None;
        };
        let relative = path.strip_prefix(&self.root).ok()?;
        Some(relative.to_string_lossy().replace('\\', "/"))
    }
}

// ─── Cones of a plan ─────────────────────────────────────────────────────────

/// Computes the dependency cones of one plan's pipeline files. Everything read or parsed for
/// one step is kept for the next.
pub struct ProjectCones {
    root: PathBuf,
    files: Rc<SourceFiles>,
    /// The plan's pipeline files, in the order given.
    pipelines: Vec<Rc<PipelineFile>>,
    /// The same files by stem, each list ordered by directory, then as given.
    by_stem: Rc<HashMap<String, Vec<Rc<PipelineFile>>>>,
    /// [`package_of`] each pipeline directory, asked once.
    packages: HashMap<PathBuf, Option<String>>,
    /// Pipeline files that share a directory and a layout share an import path, and with it
    /// every helper module already resolved. Two `helpers.py` in different directories never do.
    import_paths: HashMap<(PathBuf, Vec<PathBuf>), ImportPath>,
    /// For each pipeline file (canonical path), the first project module it imports, directly
    /// or through other project modules, that does not parse ([`ProjectCones::validate_imports`]).
    broken: std::collections::BTreeMap<PathBuf, BrokenImport>,
}

/// A project module a pipeline file imports that does not parse (#550). The worker's import of
/// the pipeline file would fail, so no step of that file can run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenImport {
    /// The module's file, relative to the project root when it is under it.
    pub file: String,
    /// The file whose `import` reaches it: the pipeline file or another project module.
    pub imported_by: String,
    pub problem: crate::parse::SyntaxProblem,
}

impl ProjectCones {
    /// `root` is the project root: the working directory of barca and of its workers.
    /// `pipeline_files` are the plan's files with their text, already read.
    pub fn new<'f>(
        root: &Path,
        pipeline_files: impl IntoIterator<Item = (&'f Path, Rc<str>)>,
    ) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let files = SourceFiles::default();
        // One `realpath` per directory rather than per file.
        let mut canonical_dirs: HashMap<PathBuf, PathBuf> = HashMap::new();
        let mut pipelines = Vec::new();
        for (path, source) in pipeline_files {
            let path = canonical_file(path, &mut canonical_dirs);
            files.insert(path.clone(), source.clone());
            pipelines.push(Rc::new(PipelineFile {
                dir: path.parent().unwrap_or(&root).to_path_buf(),
                stem: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path,
                source,
            }));
        }
        let mut by_directory = pipelines.clone();
        by_directory.sort_by(|a, b| a.dir.cmp(&b.dir));
        let mut by_stem: HashMap<String, Vec<Rc<PipelineFile>>> = HashMap::new();
        for file in by_directory {
            by_stem.entry(file.stem.clone()).or_default().push(file);
        }
        ProjectCones {
            root,
            files: Rc::new(files),
            pipelines,
            by_stem: Rc::new(by_stem),
            packages: HashMap::new(),
            import_paths: HashMap::new(),
            broken: std::collections::BTreeMap::new(),
        }
    }

    /// The `index`th pipeline file given to [`ProjectCones::new`], ready to hash the cones of
    /// its functions.
    pub fn pipeline(&mut self, index: usize) -> PipelineCones<'_> {
        let file = self.pipelines[index].clone();
        let package = self
            .packages
            .entry(file.dir.clone())
            .or_insert_with(|| package_of(&file.dir, &self.root));
        let layout = PipelineLayout::in_dir(&file.path, &self.root, package.as_deref());
        let modules = self
            .import_paths
            .entry((file.dir.clone(), layout.path.clone()))
            .or_insert_with(|| ImportPath {
                dir: file.dir.clone(),
                path: layout.path,
                root: self.root.clone(),
                by_stem: self.by_stem.clone(),
                files: self.files.clone(),
                found: RefCell::new(HashMap::new()),
            });
        PipelineCones {
            module: Module::new(layout.module, file.source.clone(), layout.is_package),
            modules,
        }
    }

    /// Refuse source selections that a shared Python process cannot make
    /// consistently. External modules remain external: off-path stems alone
    /// provide no evidence that an ordinary installed/stdlib import is invalid.
    pub(crate) fn validate_imports(
        &mut self,
        nodes: &[Vec<crate::model::ExtractedNode>],
    ) -> std::collections::BTreeMap<PathBuf, String> {
        let mut errors = std::collections::BTreeMap::new();
        let mut broken = std::collections::BTreeMap::new();
        type ImportSites =
            std::collections::BTreeMap<Option<PathBuf>, std::collections::BTreeSet<PathBuf>>;
        let mut bindings: std::collections::BTreeMap<String, ImportSites> =
            std::collections::BTreeMap::new();
        type IdentitySites =
            std::collections::BTreeMap<String, std::collections::BTreeSet<PathBuf>>;
        let mut identities: std::collections::HashMap<PathBuf, IdentitySites> = self
            .pipelines
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    std::collections::BTreeMap::from([(
                        qualified_path(&file.path, &self.root),
                        std::collections::BTreeSet::new(),
                    )]),
                )
            })
            .collect();
        for (index, source_nodes) in nodes.iter().enumerate() {
            let source = self.pipelines[index].path.clone();
            let pipeline = self.pipeline(index);
            let root = pipeline.modules.root.clone();
            let label = |file: &Path| {
                file.strip_prefix(&root)
                    .unwrap_or(file)
                    .to_string_lossy()
                    .replace('\\', "/")
            };
            // (module, its file, the file that imports it); the pipeline file has no importer.
            let mut pending = vec![(Rc::new(pipeline.module), source.clone(), None::<PathBuf>)];
            let mut visited = std::collections::HashSet::new();
            while let Some((module, module_file, importer)) = pending.pop() {
                if !visited.insert(module.name.clone()) {
                    continue;
                }
                let imports = module.import_modules();
                if let Some(importer) = importer
                    && let Some(problem) = module.syntax_error()
                    && !broken.contains_key(&source)
                {
                    broken.insert(
                        source.clone(),
                        BrokenImport {
                            file: label(&module_file),
                            imported_by: label(&importer),
                            problem: problem.clone(),
                        },
                    );
                }
                for name in imports {
                    let (selected, imported) = match pipeline.modules.find(&name) {
                        Found::Module(module, _, file) => {
                            let file = file.canonicalize().unwrap_or(file);
                            for (prior, prior_sources) in
                                identities.get(&file).into_iter().flatten()
                            {
                                if *prior != name {
                                    let error = format!(
                                        "project source '{}' has conflicting import identities '{prior}' and '{name}' from '{}'; use the explicit qualified import `from {} import <name>` so setup and pickle identity do not depend on worker history",
                                        file.display(),
                                        source.display(),
                                        qualified_path(&file, &pipeline.modules.root),
                                    );
                                    errors
                                        .entry(source.clone())
                                        .or_insert_with(|| error.clone());
                                    for prior_source in prior_sources {
                                        errors
                                            .entry(prior_source.clone())
                                            .or_insert_with(|| error.clone());
                                    }
                                }
                            }
                            identities
                                .entry(file.clone())
                                .or_default()
                                .entry(name.clone())
                                .or_default()
                                .insert(source.clone());
                            (Some(file.clone()), Some((module, file)))
                        }
                        _ => (None, None),
                    };
                    for (prior, prior_sources) in bindings.get(&name).into_iter().flatten() {
                        if *prior != selected {
                            let describe = |file: &Option<PathBuf>| {
                                file.as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_else(|| "external or unavailable module".into())
                            };
                            let project = selected
                                .as_ref()
                                .or(prior.as_ref())
                                .expect("different bindings include a project source");
                            let error = format!(
                                "project import '{name}' resolves to '{}' from '{}' and '{}' from '{}'; use explicit qualified imports for the project source (for example `from {} import <name>`) instead of sharing a history-dependent module name",
                                describe(prior),
                                prior_sources
                                    .first()
                                    .expect("an import binding has a source")
                                    .display(),
                                describe(&selected),
                                source.display(),
                                qualified_path(project, &pipeline.modules.root),
                            );
                            if prior.is_some() {
                                for prior_source in prior_sources {
                                    errors
                                        .entry(prior_source.clone())
                                        .or_insert_with(|| error.clone());
                                }
                            }
                            if selected.is_some() {
                                errors.entry(source.clone()).or_insert(error);
                            }
                        }
                    }
                    bindings
                        .entry(name)
                        .or_default()
                        .entry(selected)
                        .or_default()
                        .insert(source.clone());
                    if let Some((imported, file)) = imported {
                        pending.push((imported, file, Some(module_file.clone())));
                    }
                }
            }
            for node in source_nodes {
                for reference in node.inputs.iter().map(|input| &input.upstream).chain(
                    node.partitions.values().filter_map(|p| match p {
                        crate::model::PartitionSpec::DerivedFrom { source_ref } => Some(source_ref),
                        _ => None,
                    }),
                ) {
                    if let crate::model::NodeRef::Imported { module, name } = reference
                        && matches!(
                            pipeline.modules.find(module),
                            Found::Missing | Found::Namespace(_)
                        )
                        && let Some(file) = pipeline.modules.pipelines_named(module).next()
                    {
                        errors.entry(source.clone()).or_insert_with(|| format!(
                            "input '{name}' in '{}' imports off-path project source '{}' as '{module}'; use an explicit qualified import: `from {} import {name}`",
                            source.display(),
                            file.path.display(),
                            qualified_path(&file.path, &pipeline.modules.root),
                        ));
                    }
                }
            }
        }
        self.broken = broken;
        errors
    }

    /// The project module that does not parse that the `index`th pipeline file imports, if
    /// any. Known after [`ProjectCones::validate_imports`].
    pub(crate) fn broken_import(&self, index: usize) -> Option<&BrokenImport> {
        self.broken.get(&self.pipelines[index].path)
    }

    /// The files read from disk so far (pipeline files are given, not read).
    #[cfg(test)]
    fn files_read(&self) -> Vec<PathBuf> {
        self.files.read.borrow().clone()
    }

    /// Every path looked up while loading, whether a file was there or not, sorted.
    pub(crate) fn paths_probed(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = self.files.sources.borrow().keys().cloned().collect();
        paths.sort();
        paths
    }
}

fn qualified_path(file: &Path, root: &Path) -> String {
    let relative = file.strip_prefix(root).unwrap_or(file).with_extension("");
    let mut parts = relative
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if parts.last().is_some_and(|part| part == "__init__") {
        parts.pop();
    }
    parts.join(".")
}

/// `path` with symlinks resolved, as the worker resolves it before importing.
fn canonical_file(path: &Path, canonical_dirs: &mut HashMap<PathBuf, PathBuf>) -> PathBuf {
    let is_symlink = path.symlink_metadata().is_ok_and(|m| m.is_symlink());
    let (Some(name), false) = (path.file_name(), is_symlink) else {
        return path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    };
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    canonical_dirs
        .entry(dir.to_path_buf())
        .or_insert_with(|| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()))
        .join(name)
}

/// One pipeline file and the modules it can import.
pub struct PipelineCones<'a> {
    module: Module,
    modules: &'a ImportPath,
}

impl PipelineCones<'_> {
    /// Literal project exports, through the worker's import path and shared source cache.
    pub(crate) fn star_exports(
        &self,
        import: &ruff_python_ast::StmtImportFrom,
    ) -> Option<Vec<String>> {
        let name = self.module.star_import_module(import)?;
        let module = self.modules.module(&name)?;
        module.star_exports()
    }

    /// The cone hash of a top-level function of the file ([`cone::cone_hash`]).
    pub fn hash(&self, function_name: &str) -> String {
        cone::cone_hash(&self.module, function_name, self.modules)
    }

    /// [`Self::hash`] and the digest of each definition in the cone
    /// ([`cone::cone_hash_and_parts`]).
    pub fn hash_and_parts(&self, function_name: &str) -> (String, Vec<(String, String)>) {
        cone::cone_hash_and_parts(&self.module, function_name, self.modules)
    }

    /// [`Self::hash_and_parts`], and the environment variables the definitions of the cone
    /// read ([`crate::env_reads`]). `scanned` keeps each source's reads across the steps of a
    /// load, so a helper reached by many steps is parsed once.
    pub fn hash_parts_and_env_reads(
        &self,
        function_name: &str,
        scanned: &mut HashMap<Rc<str>, Rc<BTreeSet<String>>>,
    ) -> (String, Vec<(String, String)>, BTreeSet<String>) {
        let (hash, parts, sources) =
            cone::cone_hash_parts_and_sources(&self.module, function_name, self.modules);
        let mut reads = BTreeSet::new();
        for source in sources {
            let found = scanned
                .entry(source.clone())
                .or_insert_with(|| Rc::new(crate::env_reads::in_source(&source)));
            reads.extend(found.iter().cloned());
        }
        (hash, parts, reads)
    }
}

#[cfg(test)]
mod tests;
