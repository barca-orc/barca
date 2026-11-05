use super::*;
use std::fs;

/// A project on disk. `root` is canonical, as `build_dag_blocking` passes it.
struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

const HELPER: &str = "def compute():\n    return 1\n\n\ndef unused():\n    return 0\n";
const USES_HELPERS: &str = "from helpers import compute\n\n\ndef step():\n    return compute()\n";

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

/// What [`ProjectCones::validate_imports`] records for each of `pipelines`: the broken
/// module, its importer and the position, or `None`.
fn broken(project: &Project, pipelines: &[&str]) -> Vec<Option<(String, String, String)>> {
    let sources: Vec<(PathBuf, Rc<str>)> = pipelines
        .iter()
        .map(|rel| {
            let path = project.root.join(rel);
            (path.clone(), fs::read_to_string(&path).unwrap().into())
        })
        .collect();
    let mut cones = ProjectCones::new(
        &project.root,
        sources.iter().map(|(p, s)| (p.as_path(), s.clone())),
    );
    let nodes = vec![Vec::new(); sources.len()];
    cones.validate_imports(&nodes);
    (0..sources.len())
        .map(|index| {
            cones
                .broken_import(index)
                .map(|b| (b.file.clone(), b.imported_by.clone(), b.problem.to_string()))
        })
        .collect()
}

#[test]
fn a_helper_that_does_not_parse_is_recorded_for_the_files_that_import_it() {
    let project = Project::new(&[
        (
            "helpers.py",
            "def compute()
    return 1
",
        ),
        ("uses.py", USES_HELPERS),
        (
            "other.py",
            "import json


def step():
    return json.dumps(1)
",
        ),
        ("pkg/__init__.py", ""),
        (
            "pkg/deep.py",
            "from helpers import compute
",
        ),
        (
            "indirect.py",
            "def step():
    from pkg.deep import compute
    return compute()
",
        ),
    ]);
    let found = broken(&project, &["uses.py", "other.py", "indirect.py"]);
    let position = "line 1, column 14: Expected `:`, found newline".to_string();
    assert_eq!(
        found[0],
        Some(("helpers.py".into(), "uses.py".into(), position.clone()))
    );
    assert_eq!(
        found[1], None,
        "a file that does not import it is not affected"
    );
    // Through another project module, from an import inside a function.
    assert_eq!(
        found[2],
        Some(("helpers.py".into(), "pkg/deep.py".into(), position))
    );
}

#[test]
fn the_excerpt_points_at_the_column() {
    let source = "x = 1
def normalize(rows)
    return rows
";
    let error = ruff_python_parser::parse_module(source).unwrap_err();
    let problem = crate::parse::SyntaxProblem::of(source, &error);
    assert_eq!((problem.line, problem.column), (2, 20));
    assert_eq!(
        problem.excerpt(),
        "    def normalize(rows)\n                       ^"
    );
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
