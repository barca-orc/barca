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
use std::collections::HashMap;
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
        type ImportSites =
            std::collections::BTreeMap<Option<PathBuf>, std::collections::BTreeSet<PathBuf>>;
        let mut bindings: std::collections::BTreeMap<String, ImportSites> =
            std::collections::BTreeMap::new();
        let mut identities: std::collections::HashMap<PathBuf, (String, Option<PathBuf>)> = self
            .pipelines
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    (qualified_path(&file.path, &self.root), None),
                )
            })
            .collect();
        for (index, source_nodes) in nodes.iter().enumerate() {
            let source = self.pipelines[index].path.clone();
            let pipeline = self.pipeline(index);
            let mut pending = vec![Rc::new(pipeline.module)];
            let mut visited = std::collections::HashSet::new();
            while let Some(module) = pending.pop() {
                if !visited.insert(module.name.clone()) {
                    continue;
                }
                for name in module.import_modules() {
                    let (selected, imported) = match pipeline.modules.find(&name) {
                        Found::Module(module, _, file) => {
                            let file = file.canonicalize().unwrap_or(file);
                            if let Some((prior, prior_source)) = identities.get(&file) {
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
                                    if let Some(prior_source) = prior_source {
                                        errors.entry(prior_source.clone()).or_insert(error);
                                    }
                                }
                            } else {
                                identities
                                    .insert(file.clone(), (name.clone(), Some(source.clone())));
                            }
                            (Some(file), Some(module))
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
                    if let Some(imported) = imported {
                        pending.push(imported);
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
        errors
    }

    /// The files read from disk so far (pipeline files are given, not read).
    #[cfg(test)]
    fn files_read(&self) -> Vec<PathBuf> {
        self.files.read.borrow().clone()
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
    /// The cone hash of a top-level function of the file ([`cone::cone_hash`]).
    pub fn hash(&self, function_name: &str) -> String {
        cone::cone_hash(&self.module, function_name, self.modules)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A project on disk. `root` is canonical, as `build_dag_blocking` passes it.
    struct Project {
        _tmp: tempfile::TempDir,
        root: PathBuf,
    }

    const HELPER: &str = "def compute():\n    return 1\n\n\ndef unused():\n    return 0\n";
    const USES_HELPERS: &str =
        "from helpers import compute\n\n\ndef step():\n    return compute()\n";

    impl Project {
        /// `files` are (path relative to the root, source).
        fn new(files: &[(&str, &str)]) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("root");
            fs::create_dir(&root).unwrap();
            let project = Project {
                root: root.canonicalize().unwrap(),
                _tmp: tmp,
            };
            for (rel, source) in files {
                project.write(rel, source);
            }
            project
        }

        fn write(&self, rel: &str, source: &str) {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }

        /// Edit `compute` (which the step uses) in the helper at `rel`.
        fn edit_compute(&self, rel: &str) {
            self.write(rel, &HELPER.replace("return 1", "return 22"));
        }

        /// One plan over `pipelines`: the cone hash of `step` in each, and the files read.
        fn plan(&self, pipelines: &[&str]) -> (Vec<String>, Vec<String>) {
            let sources: Vec<(PathBuf, Rc<str>)> = pipelines
                .iter()
                .map(|rel| {
                    let path = self.root.join(rel);
                    let source: Rc<str> = fs::read_to_string(&path).unwrap().into();
                    (path, source)
                })
                .collect();
            let mut cones = ProjectCones::new(
                &self.root,
                sources.iter().map(|(p, s)| (p.as_path(), s.clone())),
            );
            let hashes = (0..sources.len())
                .map(|index| cones.pipeline(index).hash("step"))
                .collect();
            let mut read: Vec<String> = cones
                .files_read()
                .iter()
                .map(|p| {
                    let rel = p.strip_prefix(&self.root).unwrap_or(p);
                    rel.to_string_lossy().replace('\\', "/")
                })
                .collect();
            read.sort();
            (hashes, read)
        }

        fn hash(&self, pipeline: &str) -> String {
            self.plan(&[pipeline]).0.remove(0)
        }
    }

    /// Editing `loaded` (the file Python imports) changes the hash; editing `shadowed` (a
    /// same-named file Python does not import) leaves it alone.
    fn assert_tracks(project: &Project, pipeline: &str, loaded: &str, shadowed: Option<&str>) {
        let before = project.hash(pipeline);
        assert!(
            !before.is_empty(),
            "{loaded} is not in the cone of {pipeline}"
        );
        if let Some(shadowed) = shadowed {
            project.edit_compute(shadowed);
            assert_eq!(
                before,
                project.hash(pipeline),
                "{shadowed} is not what {pipeline} imports, but editing it changed the hash"
            );
        }
        project.edit_compute(loaded);
        assert_ne!(
            before,
            project.hash(pipeline),
            "{loaded} is what {pipeline} imports, but editing it did not change the hash"
        );
    }

    // ─── Layouts: the import path mirrors the worker's ───────────────────────────

    #[test]
    fn layout_of_a_file_in_the_root_is_the_root_alone() {
        let p = Project::new(&[("p.py", "")]);
        let layout = PipelineLayout::of(&p.root.join("p.py"), &p.root);
        assert_eq!(layout.path, vec![p.root.clone()]);
        assert!(!layout.module.contains('.'));
    }

    #[test]
    fn layout_of_a_file_in_a_plain_subdirectory_is_its_directory_then_the_root() {
        let p = Project::new(&[("pipelines/p.py", "")]);
        let layout = PipelineLayout::of(&p.root.join("pipelines/p.py"), &p.root);
        assert_eq!(layout.path, vec![p.root.join("pipelines"), p.root.clone()]);
        assert_eq!(layout.module, "pipelines.p");
    }

    #[test]
    fn layout_of_a_file_in_a_package_is_its_dotted_name_and_the_root_alone() {
        let p = Project::new(&[
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/sub/p.py", ""),
            ("pkg/loose/p.py", ""),
            ("loose/pkg/__init__.py", ""),
            ("loose/pkg/p.py", ""),
        ]);
        let of = |rel: &str| PipelineLayout::of(&p.root.join(rel), &p.root);
        assert_eq!(
            of("pkg/sub/p.py"),
            PipelineLayout {
                module: "pkg.sub.p".into(),
                is_package: false,
                path: vec![p.root.clone()],
            }
        );
        // Package initializers share the ordinary package identity, including
        // the package directly under the root.
        assert_eq!(of("pkg/sub/__init__.py").module, "pkg.sub");
        assert!(of("pkg/sub/__init__.py").is_package);
        assert_eq!(of("pkg/__init__.py").path, vec![p.root.clone()]);
        // Every directory from the root down must be a package.
        assert_eq!(
            of("pkg/loose/p.py").path,
            vec![p.root.join("pkg/loose"), p.root.clone()]
        );
        assert_eq!(
            of("loose/pkg/p.py").path,
            vec![p.root.join("loose/pkg"), p.root.clone()]
        );
    }

    #[test]
    fn layout_of_a_file_outside_the_root_is_its_directory_then_the_root() {
        let p = Project::new(&[("p.py", "")]);
        let outside = p.root.parent().unwrap().join("elsewhere");
        fs::create_dir(&outside).unwrap();
        let layout = PipelineLayout::of(&outside.join("p.py"), &p.root);
        assert_eq!(layout.path, vec![outside, p.root.clone()]);
    }

    // ─── Resolution per layout: the loaded file is tracked, the shadowed one is not ──

    #[test]
    fn flat_directory_imports_the_module_beside_the_pipeline() {
        let p = Project::new(&[("p.py", USES_HELPERS), ("helpers.py", HELPER)]);
        assert_tracks(&p, "p.py", "helpers.py", None);
    }

    #[test]
    fn subdirectory_pipeline_imports_a_root_module() {
        let p = Project::new(&[
            ("pipelines/p.py", USES_HELPERS),
            ("helpers.py", HELPER),
            ("other/helpers.py", HELPER),
        ]);
        assert_tracks(&p, "pipelines/p.py", "helpers.py", Some("other/helpers.py"));
    }

    #[test]
    fn subdirectory_pipeline_imports_a_root_package_by_dotted_name() {
        let p = Project::new(&[
            (
                "pipelines/p.py",
                "from shared.utils import compute\n\n\ndef step():\n    return compute()\n",
            ),
            ("shared/__init__.py", ""),
            ("shared/utils.py", HELPER),
        ]);
        assert_tracks(&p, "pipelines/p.py", "shared/utils.py", None);
    }

    #[test]
    fn a_module_in_the_pipeline_directory_shadows_the_root_module() {
        let p = Project::new(&[
            ("pipelines/p.py", USES_HELPERS),
            ("pipelines/helpers.py", HELPER),
            ("helpers.py", HELPER),
        ]);
        assert_tracks(
            &p,
            "pipelines/p.py",
            "pipelines/helpers.py",
            Some("helpers.py"),
        );
    }

    #[test]
    fn package_pipeline_imports_a_bare_name_from_the_root_not_from_its_own_directory() {
        // `pkg/p.py` runs as `pkg.p` with only the root on the import path: `import helpers`
        // is `<root>/helpers.py`, and `pkg/helpers.py` is `pkg.helpers`.
        let p = Project::new(&[
            ("pkg/__init__.py", ""),
            ("pkg/p.py", USES_HELPERS),
            ("pkg/helpers.py", HELPER),
            ("helpers.py", HELPER),
        ]);
        assert_tracks(&p, "pkg/p.py", "helpers.py", Some("pkg/helpers.py"));
    }

    #[test]
    fn package_pipeline_imports_a_sibling_by_relative_and_by_dotted_name() {
        for import in [
            "from .helpers import compute",
            "from pkg.helpers import compute",
        ] {
            let p = Project::new(&[
                ("pkg/__init__.py", ""),
                (
                    "pkg/p.py",
                    &format!("{import}\n\n\ndef step():\n    return compute()\n"),
                ),
                ("pkg/helpers.py", HELPER),
                ("helpers.py", HELPER),
            ]);
            assert_tracks(&p, "pkg/p.py", "pkg/helpers.py", Some("helpers.py"));
        }
    }

    #[test]
    fn a_package_wins_over_a_module_of_the_same_name_in_one_directory() {
        let p = Project::new(&[
            ("p.py", USES_HELPERS),
            ("helpers/__init__.py", HELPER),
            ("helpers.py", HELPER),
        ]);
        assert_tracks(&p, "p.py", "helpers/__init__.py", Some("helpers.py"));
    }

    #[test]
    fn a_module_wins_over_a_directory_without_init_earlier_on_the_path() {
        // `pipelines/helpers/` has no `__init__.py`: it is a namespace package only if no path
        // entry has a real `helpers`, and the root has one.
        let p = Project::new(&[
            ("pipelines/p.py", USES_HELPERS),
            ("pipelines/helpers/compute.py", HELPER),
            ("helpers.py", HELPER),
        ]);
        assert_tracks(
            &p,
            "pipelines/p.py",
            "helpers.py",
            Some("pipelines/helpers/compute.py"),
        );
    }

    #[test]
    fn a_directory_without_init_is_a_namespace_package() {
        let p = Project::new(&[
            (
                "p.py",
                "from lib.utils import compute\n\n\ndef step():\n    return compute()\n",
            ),
            ("lib/utils.py", HELPER),
        ]);
        assert_tracks(&p, "p.py", "lib/utils.py", None);
    }

    #[test]
    fn a_module_outside_the_root_is_not_followed() {
        // `<root>/../shared.py` is importable only through a `sys.path` entry the user adds.
        let p = Project::new(&[(
            "pipelines/p.py",
            "from shared import compute\n\n\ndef step():\n    return compute()\n",
        )]);
        let outside = p.root.parent().unwrap().join("shared.py");
        fs::write(&outside, HELPER).unwrap();
        let (before, read) = p.plan(&["pipelines/p.py"]);
        fs::write(&outside, HELPER.replace("return 1", "return 22")).unwrap();
        assert_eq!(before, p.plan(&["pipelines/p.py"]).0);
        assert!(read.is_empty(), "read {read:?}");
    }

    #[test]
    fn same_named_helpers_in_two_pipeline_directories_are_hashed_separately() {
        let p = Project::new(&[
            ("east/p.py", USES_HELPERS),
            ("east/helpers.py", HELPER),
            ("west/p.py", USES_HELPERS),
            ("west/helpers.py", HELPER),
        ]);
        let before = p.plan(&["east/p.py", "west/p.py"]).0;
        p.edit_compute("west/helpers.py");
        let after = p.plan(&["east/p.py", "west/p.py"]).0;
        assert_eq!(
            before[0], after[0],
            "east/p.py does not import west/helpers.py"
        );
        assert_ne!(before[1], after[1]);
    }

    #[test]
    fn off_path_pipeline_files_do_not_hide_namespace_packages() {
        // An off-path pipeline stem cannot replace the normal namespace package.
        // Editing that off-path file must not change this unrelated cone.
        let p = Project::new(&[
            (
                "a/p.py",
                "from shared import compute\n\n\ndef step():\n    return compute()\n",
            ),
            ("b/shared.py", HELPER),
            ("shared/notes.py", HELPER),
        ]);
        let plan = ["a/p.py", "b/shared.py"];
        let before = p.plan(&plan).0[0].clone();
        p.edit_compute("shared/notes.py");
        assert_eq!(before, p.plan(&plan).0[0]);
        p.edit_compute("b/shared.py");
        assert_eq!(before, p.plan(&plan).0[0]);
    }

    #[test]
    fn a_directory_without_init_is_still_a_namespace_package_beside_other_pipelines() {
        // The by-stem fallback must not hide submodules of a namespace package when no
        // pipeline file has its name.
        let p = Project::new(&[
            (
                "a/p.py",
                "from lib.utils import compute\n\n\ndef step():\n    return compute()\n",
            ),
            ("b/other.py", HELPER),
            ("lib/utils.py", HELPER),
        ]);
        let plan = ["a/p.py", "b/other.py"];
        let before = p.plan(&plan).0[0].clone();
        p.edit_compute("lib/utils.py");
        assert_ne!(before, p.plan(&plan).0[0]);
    }

    #[test]
    fn a_module_beside_the_pipeline_wins_over_a_directory_without_init_beside_it() {
        let p = Project::new(&[
            ("pipelines/p.py", USES_HELPERS),
            ("pipelines/helpers.py", HELPER),
            ("pipelines/helpers/compute.py", HELPER),
            ("helpers/compute.py", HELPER),
        ]);
        assert_tracks(
            &p,
            "pipelines/p.py",
            "pipelines/helpers.py",
            Some("pipelines/helpers/compute.py"),
        );
    }

    #[test]
    fn a_helper_is_parsed_once_per_plan_across_import_paths() {
        let p = Project::new(&[
            ("a/p.py", USES_HELPERS),
            ("b/p.py", USES_HELPERS),
            ("helpers.py", HELPER),
        ]);
        let sources: Vec<(PathBuf, Rc<str>)> = ["a/p.py", "b/p.py"]
            .iter()
            .map(|rel| (p.root.join(rel), Rc::from(USES_HELPERS)))
            .collect();
        let mut cones = ProjectCones::new(
            &p.root,
            sources.iter().map(|(p, s)| (p.as_path(), s.clone())),
        );
        let first = cones.pipeline(0).modules.module("helpers").unwrap();
        let second = cones.pipeline(1).modules.module("helpers").unwrap();
        assert!(
            Rc::ptr_eq(&first, &second),
            "two import paths parsed helpers.py separately"
        );
    }

    #[test]
    fn off_path_pipeline_stems_do_not_change_the_cone() {
        // Merely including another pipeline never makes its directory importable.
        let p = Project::new(&[("a/p.py", USES_HELPERS), ("b/helpers.py", HELPER)]);
        let before = p.plan(&["a/p.py", "b/helpers.py"]).0[0].clone();
        p.edit_compute("b/helpers.py");
        assert_eq!(before, p.plan(&["a/p.py", "b/helpers.py"]).0[0]);
        // Not in the plan: nothing makes `b/` importable.
        assert_eq!(
            p.hash("a/p.py"),
            Project::new(&[("a/p.py", USES_HELPERS)]).hash("a/p.py")
        );
    }

    // ─── A top-level name that is also another pipeline file's stem ──────────────

    /// Ordinary import resolution is independent of unrelated pipeline stems:
    /// edits track only sources on this context's own Python import path.
    #[test]
    fn a_name_shared_with_another_pipeline_file_follows_what_the_worker_can_load() {
        const MODULE: &str = "def name():\n    return 1\n";
        // (form, reads `shared.sub` rather than an attribute of `shared`, pipeline source)
        let forms: [(&str, bool, &str); 5] = [
            (
                "import N",
                false,
                "import shared\n\n\ndef step():\n    return shared.name()\n",
            ),
            (
                "from N import attr",
                false,
                "from shared import name\n\n\ndef step():\n    return name()\n",
            ),
            (
                "import N.sub",
                true,
                "import shared.sub\n\n\ndef step():\n    return shared.sub.name()\n",
            ),
            (
                "from N import sub",
                true,
                "from shared import sub\n\n\ndef step():\n    return sub.name()\n",
            ),
            (
                "from N.sub import name",
                true,
                "from shared.sub import name\n\n\ndef step():\n    return name()\n",
            ),
        ];
        // What `shared` is in the root, which is on every pipeline's import path.
        let kinds: [(&str, &[&str]); 3] = [
            ("regular package", &["shared/__init__.py", "shared/sub.py"]),
            ("directory without init", &["shared/sub.py"]),
            ("nothing", &[]),
        ];
        let mut cells = 0;
        for pipeline in ["p.py", "a/p.py", "pkg/p.py"] {
            for (kind, kind_files) in kinds {
                for with_stem in [false, true] {
                    for (form, sub_form, source) in forms {
                        let mut files: Vec<(String, &str)> =
                            kind_files.iter().map(|f| (f.to_string(), MODULE)).collect();
                        files.push((pipeline.to_string(), source));
                        if pipeline == "pkg/p.py" {
                            files.push(("pkg/__init__.py".to_string(), ""));
                        }
                        let mut plan = vec![pipeline];
                        if with_stem {
                            files.push(("b/shared.py".to_string(), MODULE));
                            plan.push("b/shared.py");
                        }
                        let borrowed: Vec<(&str, &str)> =
                            files.iter().map(|(f, s)| (f.as_str(), *s)).collect();
                        let project = Project::new(&borrowed);

                        let expected: Vec<&str> = match (kind, sub_form) {
                            ("regular package", true) => vec!["shared/sub.py"],
                            ("regular package", false) => vec!["shared/__init__.py"],
                            ("directory without init", true) => vec!["shared/sub.py"],
                            _ => vec![],
                        };

                        let (before, read) = project.plan(&plan);
                        let candidates = ["shared/__init__.py", "shared/sub.py", "b/shared.py"];
                        for file in candidates {
                            if !files.iter().any(|(f, _)| f == file) {
                                continue;
                            }
                            project.write(file, &MODULE.replace("return 1", "return 22"));
                            let changed = project.plan(&plan).0[0] != before[0];
                            project.write(file, MODULE);
                            assert_eq!(
                                changed,
                                expected.contains(&file),
                                "{pipeline}, `shared` is {kind}, b/shared.py: {with_stem}, \
                                 `{form}`: editing {file}"
                            );
                        }
                        assert!(
                            read.iter().all(|f| candidates.contains(&f.as_str())),
                            "{pipeline}, {kind}, {form}: read {read:?}"
                        );
                        cells += 1;
                    }
                }
            }
        }
        assert_eq!(cells, 3 * 3 * 2 * 5);
    }

    #[test]
    fn a_name_in_the_pipeline_directory_follows_the_same_table() {
        const MODULE: &str = "def name():\n    return 1\n";
        let attr = "from shared import name\n\n\ndef step():\n    return name()\n";
        let sub = "from shared.sub import name\n\n\ndef step():\n    return name()\n";
        let edited = MODULE.replace("return 1", "return 22");
        let changes = |project: &Project, plan: &[&str], file: &str| {
            let before = project.plan(plan).0[0].clone();
            project.write(file, &edited);
            let changed = project.plan(plan).0[0] != before;
            project.write(file, MODULE);
            changed
        };

        // A module beside a subdirectory pipeline shadows a root directory without init, and
        // makes `shared.sub` unimportable. A package pipeline does not see its own directory.
        for (pipeline, own, source, expected) in [
            ("a/p.py", "a/shared.py", attr, &["a/shared.py"][..]),
            ("a/p.py", "a/shared.py", sub, &[][..]),
            ("pkg/p.py", "pkg/shared.py", attr, &[][..]),
            ("pkg/p.py", "pkg/shared.py", sub, &["shared/sub.py"][..]),
        ] {
            let project = Project::new(&[
                (pipeline, source),
                ("pkg/__init__.py", ""),
                (own, MODULE),
                ("shared/sub.py", MODULE),
            ]);
            for file in [own, "shared/sub.py"] {
                let tracked = expected.contains(&file);
                assert_eq!(
                    changes(&project, &[pipeline], file),
                    tracked,
                    "{pipeline} {file}"
                );
            }
        }

        // A directory without init beside the pipeline, and `b/shared.py`.
        for (pipeline, source, expected) in [
            ("a/p.py", attr, &[][..]),
            ("a/p.py", sub, &["a/shared/sub.py"][..]),
            ("pkg/p.py", attr, &[][..]),
            ("pkg/p.py", sub, &[][..]),
        ] {
            let own_sub = pipeline.replace("p.py", "shared/sub.py");
            let project = Project::new(&[
                (pipeline, source),
                ("pkg/__init__.py", ""),
                (&own_sub, MODULE),
                ("b/shared.py", MODULE),
            ]);
            let plan = [pipeline, "b/shared.py"];
            for file in [own_sub.as_str(), "b/shared.py"] {
                let tracked = expected.contains(&file);
                assert_eq!(changes(&project, &plan, file), tracked, "{pipeline} {file}");
            }
        }
    }

    #[test]
    fn ordinary_import_hashes_are_relative_to_the_root() {
        // The hash must not depend on where the project is checked out.
        let files = [
            (
                "a/p.py",
                "from shared import name\n\n\ndef step():\n    return name()\n",
            ),
            ("shared.py", "def name():\n    return 1\n"),
            ("b/shared.py", "def name():\n    return 2\n"),
        ];
        let plan = ["a/p.py", "b/shared.py"];
        let first = Project::new(&files).plan(&plan).0;
        let second = Project::new(&files).plan(&plan).0;
        assert_eq!(first[0], second[0]);
    }

    // ─── Cost: only what is imported is read, once ───────────────────────────────

    #[test]
    fn only_imported_project_files_are_read() {
        let p = Project::new(&[
            (
                "pipelines/p.py",
                "import json\nimport numpy as np\nfrom helpers import compute\n\n\ndef step():\n    return json.dumps(np.array(compute()))\n",
            ),
            ("helpers.py", HELPER),
            ("unrelated.py", HELPER),
            ("pipelines/unrelated.py", HELPER),
            ("pipelines/deep/er/unrelated.py", HELPER),
            // Installed packages inside the project, in a virtualenv with or without a dot.
            (
                "venv/lib/python3.12/site-packages/numpy/__init__.py",
                "def array(x):\n    return x\n",
            ),
            (
                ".venv/lib/python3.12/site-packages/numpy/__init__.py",
                "def array(x):\n    return x\n",
            ),
            ("node_modules/pkg/setup.py", HELPER),
        ]);
        let (_, read) = p.plan(&["pipelines/p.py"]);
        assert_eq!(read, ["helpers.py"]);
    }

    #[test]
    fn installed_packages_and_the_standard_library_do_not_change_the_hash() {
        let pipeline = "import json\nimport numpy as np\nfrom requests import get as fetch\n\n\ndef step():\n    return json.dumps(np.array(fetch(json)))\n";
        let p = Project::new(&[
            ("p.py", pipeline),
            (
                "venv/lib/python3.12/site-packages/numpy/__init__.py",
                "def array(x):\n    return x\n",
            ),
            (
                "venv/lib/python3.12/site-packages/requests.py",
                "def get(x):\n    return x\n",
            ),
        ]);
        let (before, read) = p.plan(&["p.py"]);
        p.write(
            "venv/lib/python3.12/site-packages/numpy/__init__.py",
            "def array(x):\n    return 2\n",
        );
        p.write(
            "venv/lib/python3.12/site-packages/requests.py",
            "def get(x):\n    return 2\n",
        );
        assert_eq!(before, p.plan(&["p.py"]).0);
        assert!(read.is_empty(), "read {read:?}");
    }

    #[test]
    fn a_helper_is_read_once_per_plan_however_many_pipelines_import_it() {
        let p = Project::new(&[
            ("a/p.py", USES_HELPERS),
            ("a/q.py", USES_HELPERS),
            ("b/p.py", USES_HELPERS),
            ("pkg/__init__.py", ""),
            ("pkg/p.py", USES_HELPERS),
            ("helpers.py", HELPER),
        ]);
        let (hashes, read) = p.plan(&["a/p.py", "a/q.py", "b/p.py", "pkg/p.py"]);
        assert!(hashes.iter().all(|h| !h.is_empty() && *h == hashes[0]));
        assert_eq!(read, ["helpers.py"]);
    }

    #[test]
    fn a_pipeline_file_imported_by_another_is_not_read_again() {
        let p = Project::new(&[("p.py", USES_HELPERS), ("helpers.py", HELPER)]);
        let (hashes, read) = p.plan(&["p.py", "helpers.py"]);
        assert!(!hashes[0].is_empty());
        assert!(read.is_empty(), "read {read:?}");
    }
}
