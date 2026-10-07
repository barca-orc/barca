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
//! - **Any other** pipeline file is loaded by path with **its own directory, then the root**,
//!   on the import path, so a module beside the file shadows a same-named one at the root.
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
    /// Files actually read from disk, in order.
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
        if source.is_some() {
            self.read.borrow_mut().push(path.to_path_buf());
        }
        self.sources
            .borrow_mut()
            .insert(path.to_path_buf(), source.clone());
        source
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
    /// Mirrors `_worker.py::package_module_name`: a file is imported by dotted name when that
    /// name has at least two segments and every directory above the file is a package.
    fn in_dir(file: &Path, root: &Path, dir_package: Option<&str>) -> Self {
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let is_init = stem == "__init__";
        let module = match dir_package {
            Some(package) if !is_init => Some(format!("{package}.{stem}")),
            Some(package) if package.contains('.') => Some(package.to_string()),
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
            module: stem.replace('.', "_"),
            is_package: false,
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

/// A pipeline file of the plan, importable by its stem from pipeline files in other directories
/// when nothing on their import path has that name.
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
    Module(Rc<Module>, Option<PathBuf>),
    /// A namespace package: directories without `__init__.py`, one per path entry.
    Namespace(Vec<PathBuf>),
    Missing,
}

/// The modules importable from the pipeline files that share one import path.
struct ImportPath {
    /// The directory of the pipeline files importing through this path.
    dir: PathBuf,
    path: Vec<PathBuf>,
    pipelines: Rc<[Rc<PipelineFile>]>,
    files: Rc<SourceFiles>,
    found: RefCell<HashMap<String, Found>>,
}

impl ImportPath {
    fn find(&self, name: &str) -> Found {
        if let Some(found) = self.found.borrow().get(name) {
            return found.clone();
        }
        let found = match name.rsplit_once('.') {
            None => match self.search(name, name, &self.path) {
                Found::Missing => self.other_pipeline(name),
                found => found,
            },
            Some((parent, leaf)) => match self.find(parent) {
                Found::Module(_, Some(dir)) => self.search(name, leaf, &[dir]),
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
            if let Some(source) = self.files.get(&package_dir.join("__init__.py")) {
                let module = Module::new(name, source, true);
                return Found::Module(Rc::new(module), Some(package_dir));
            }
            if let Some(source) = self.files.get(&dir.join(format!("{leaf}.py"))) {
                return Found::Module(Rc::new(Module::new(name, source, false)), None);
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

    /// A pipeline file in another directory, by its stem. Every pipeline directory a worker has
    /// loaded from stays on its `sys.path` (`barca docs discovery`, "Node ids"), so the name can
    /// resolve at run time; it is tried last, as that path is searched last.
    fn other_pipeline(&self, name: &str) -> Found {
        self.pipelines
            .iter()
            .find(|p| p.stem == name && p.dir != self.dir)
            .map_or(Found::Missing, |p| {
                Found::Module(Rc::new(Module::new(name, p.source.clone(), false)), None)
            })
    }
}

impl ModuleSource for ImportPath {
    fn module(&self, name: &str) -> Option<Rc<Module>> {
        match self.find(name) {
            Found::Module(module, _) => Some(module),
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
    /// The same files in the order the by-stem fallback tries them: by directory, then as given.
    by_directory: Rc<[Rc<PipelineFile>]>,
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
        ProjectCones {
            root,
            files: Rc::new(files),
            pipelines,
            by_directory: by_directory.into(),
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
                pipelines: self.by_directory.clone(),
                files: self.files.clone(),
                found: RefCell::new(HashMap::new()),
            });
        PipelineCones {
            module: Module::new(layout.module, file.source.clone(), layout.is_package),
            modules,
        }
    }

    /// The files read from disk so far (pipeline files are given, not read).
    pub fn files_read(&self) -> Vec<PathBuf> {
        self.files.read.borrow().clone()
    }
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
        assert!(!layout.module.contains('.'));
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
        // A package's `__init__.py` is the package; directly under the root it is loaded by
        // path, like any file whose dotted name would have one segment.
        assert_eq!(of("pkg/sub/__init__.py").module, "pkg.sub");
        assert!(of("pkg/sub/__init__.py").is_package);
        assert_eq!(
            of("pkg/__init__.py").path,
            vec![p.root.join("pkg"), p.root.clone()]
        );
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
    fn a_pipeline_file_in_another_directory_is_importable_by_its_stem() {
        // Both files are in the plan: `b/` is on a worker's `sys.path` once it loaded from it.
        let p = Project::new(&[("a/p.py", USES_HELPERS), ("b/helpers.py", HELPER)]);
        let before = p.plan(&["a/p.py", "b/helpers.py"]).0[0].clone();
        p.edit_compute("b/helpers.py");
        assert_ne!(before, p.plan(&["a/p.py", "b/helpers.py"]).0[0]);
        // Not in the plan: nothing makes `b/` importable.
        assert_eq!(
            p.hash("a/p.py"),
            Project::new(&[("a/p.py", USES_HELPERS)]).hash("a/p.py")
        );
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
