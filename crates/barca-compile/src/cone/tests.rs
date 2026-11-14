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
    let a = "class M:\n    def f(self):\n        return 1\n\ndef my_asset():\n    return M().f()\n";
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
    let deep = |n: u32| format!("def inner():\n    return {n}\n\n\ndef unused():\n    return 0\n");
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

#[test]
fn exception_handler_types_follow_project_helpers() {
    let project = |name: &str| sources(&[("helpers", &format!("Error = {name}\n"))]);
    for entry in [
        "import helpers\ndef my_asset():\n    try:\n        pass\n    except helpers.Error:\n        pass\n",
        "from helpers import Error\ndef my_asset():\n    try:\n        pass\n    except (KeyError, Error):\n        pass\n",
        "def my_asset():\n    from helpers import Error\n    try:\n        pass\n    except* Error:\n        pass\n",
    ] {
        assert_ne!(
            hash_of(entry, &project("ValueError"), &[]),
            hash_of(entry, &project("TypeError"), &[]),
            "{entry}"
        );
    }
    let bare =
        "import helpers\ndef my_asset():\n    try:\n        pass\n    except:\n        pass\n";
    assert_eq!(
        hash_of(bare, &project("ValueError"), &[]),
        hash_of(bare, &project("TypeError"), &[])
    );
}

#[test]
fn assignment_receivers_and_indices_follow_helpers() {
    for body in [
        "table[helpers.compute()] = 1",
        "table[helpers.compute()]: int = 1",
        "helpers.compute().value = 1",
        "table[helpers.compute()], other = (1, 2)",
        "*table[helpers.compute()], other = (1, 2)",
        "for table[helpers.compute()] in [1]:\n        pass",
        "with manager() as table[helpers.compute()]:\n        pass",
        "result = [1 for table[helpers.compute()] in [1]]",
    ] {
        for import in ["import helpers\n", ""] {
            let local = if import.is_empty() {
                "    import helpers\n"
            } else {
                ""
            };
            let entry = format!(
                "{import}\ndef my_asset():\n{local}    table = {{}}\n    {body}\n    return table\n"
            );
            assert_ne!(
                hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
                hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
                "{body}"
            );
        }
    }
    // Writing a name is not a read of its previous imported binding.
    let entry = "from helpers import compute\n\ndef my_asset():\n    compute = 1\n    return 0\n";
    assert_eq!(
        hash_of(entry, &one("helpers", HELPERS_V1), &[]),
        hash_of(entry, &one("helpers", HELPERS_V2), &[])
    );
}

#[test]
fn function_and_lambda_defaults_follow_the_enclosing_scope() {
    let project = |n: u32| sources(&[("helpers", &format!("def compute():\n    return {n}\n"))]);
    for entry in [
        "import helpers\ndef my_asset(x=helpers.compute()):\n    return x\n",
        "import helpers\nasync def my_asset(*, x=helpers.compute()):\n    return x\n",
        "from helpers import compute\ndef inner(x=compute(), /):\n    return x\ndef my_asset():\n    return inner()\n",
        "import helpers\ndef my_asset():\n    def inner(helpers=helpers.compute()):\n        return helpers\n    return inner()\n",
        "import helpers\ndef my_asset():\n    return (lambda helpers=helpers.compute(): helpers)()\n",
        // The default follows an import in the outer function even when the
        // inner parameter shadows its name.
        "def my_asset():\n    import helpers as local\n    def inner(local=local.compute()):\n        return local\n    return inner()\n",
    ] {
        assert_ne!(
            hash_of(entry, &project(1), &[]),
            hash_of(entry, &project(2), &[]),
            "{entry}"
        );
    }
    let unused = "import helpers\ndef unused(x=helpers.compute()):\n    return x\ndef my_asset():\n    return 7\n";
    assert_eq!(
        hash_of(unused, &project(1), &[]),
        hash_of(unused, &project(2), &[])
    );
}

#[test]
fn set_literal_elements_follow_project_helpers() {
    let project = |n: u32| sources(&[("helpers", &format!("def compute():\n    return {n}\n"))]);
    for entry in [
        "import helpers\ndef my_asset():\n    return {helpers.compute()}\n",
        "from helpers import compute\ndef my_asset():\n    return {compute(), 7}\n",
        "def my_asset():\n    import helpers as local\n    return {local.compute(), 7}\n",
    ] {
        assert_ne!(
            hash_of(entry, &project(1), &[]),
            hash_of(entry, &project(2), &[]),
            "{entry}"
        );
    }
    let literal = "import helpers\ndef my_asset():\n    return {1, 7}\n";
    assert_eq!(
        hash_of(literal, &project(1), &[]),
        hash_of(literal, &project(2), &[])
    );
}

#[test]
fn loop_else_blocks_follow_project_helpers() {
    for (prefix, loop_header) in [
        ("def", "for _ in ()"),
        ("def", "while False"),
        ("async def", "async for _ in source()"),
        ("def", "for _ in [1]"),
    ] {
        for import in ["import helpers\n", ""] {
            let local = if import.is_empty() {
                "    import helpers\n"
            } else {
                ""
            };
            let entry = format!(
                "{import}\n{prefix} my_asset():\n{local}    {loop_header}:\n        break\n    else:\n        return helpers.compute()\n    return 0\n"
            );
            assert_ne!(
                hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
                hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
                "{entry}"
            );
        }
    }
}

#[test]
fn match_pattern_expressions_follow_helpers_without_reading_captures() {
    for pattern in [
        "helpers.compute",
        "helpers.compute | 2",
        "helpers.compute as captured",
        "[helpers.compute, *rest]",
        "{helpers.compute: captured}",
        "helpers.compute(captured)",
        "object(value=helpers.compute)",
    ] {
        for import in ["import helpers\n", ""] {
            let local = if import.is_empty() {
                "    import helpers\n"
            } else {
                ""
            };
            let entry = format!(
                "{import}\ndef my_asset(x):\n{local}    match x:\n        case {pattern}:\n            return 1\n    return 0\n"
            );
            assert_ne!(
                hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
                hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
                "{entry}"
            );
        }
    }
    for pattern in ["compute", "[compute, *rest]", "{1: compute, **rest}"] {
        let entry = format!(
            "from helpers import compute\n\ndef my_asset(x):\n    match x:\n        case {pattern}:\n            return 1\n    return 0\n"
        );
        assert_eq!(
            hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
            hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
            "capture is not a read: {entry}"
        );
    }
}

#[test]
fn helper_decorator_expressions_are_followed_outside_the_decorated_scope() {
    for entry in [
        "from helpers import compute\n\n@compute\ndef inner(): return 1\n\ndef my_asset(): return inner()\n",
        "import helpers\n\n@wrap(helpers.compute())\ndef inner(): return 1\n\ndef my_asset(): return inner()\n",
        "def my_asset():\n    from helpers import compute\n    @compute\n    def inner(compute): return 1\n    return inner(None)\n",
        "from helpers import compute\n\n@compute\nclass Inner: pass\n\ndef my_asset(): return Inner()\n",
        "def my_asset():\n    from helpers import compute\n    @compute\n    class Inner: compute = 1\n    return Inner()\n",
    ] {
        assert_ne!(
            hash_of(entry, &one("helpers", HELPERS_V1), &[]),
            hash_of(entry, &one("helpers", HELPERS_V2), &[]),
            "{entry}"
        );
    }
}

#[test]
fn ordinary_helper_keeps_decorator_text_and_follows_its_values() {
    let pipeline = |decorator: &str, size: &str| {
        format!(
            "from barca import asset\nSIZE = {size}\n\n\n{decorator}\ndef helper():\n    return 1\n\n\n@asset()\ndef my_asset():\n    return helper()\n"
        )
    };
    let before = cone_hash(&pipeline("@lru_cache(maxsize=SIZE)", "1"), "my_asset");
    assert_ne!(
        before,
        cone_hash(&pipeline("@lru_cache( maxsize = SIZE )", "1"), "my_asset")
    );
    assert_ne!(
        before,
        cone_hash(&pipeline("@lru_cache(maxsize=SIZE)", "2"), "my_asset")
    );
}

#[test]
fn unpacked_and_augmented_module_bindings_follow_their_helper_provenance() {
    for bindings in [
        "VALUE, OTHER = compute(), 7",
        "[VALUE, OTHER] = [compute(), 7]",
        "VALUE, *REST = [compute(), 7, 8]",
        "VALUE = 0\nVALUE += compute()",
        "VALUE, OTHER = compute(), 7\nCAPTURED = VALUE\nVALUE = 0",
        "VALUE = 0\nVALUE += compute()\nCAPTURED = VALUE\nVALUE = 0",
    ] {
        let name = if bindings.contains("CAPTURED") {
            "CAPTURED"
        } else {
            "VALUE"
        };
        let entry =
            format!("from helpers import compute\n{bindings}\n\ndef my_asset(): return {name}\n");
        assert_ne!(
            hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
            hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
            "{bindings}"
        );
    }
}

#[test]
fn module_level_tuple_and_augmented_constant_edits_change_hashes() {
    // These formerly omitted definitions now conservatively contribute their module.
    let tuple = |b: u32| format!("A, B = 1, {b}\n\ndef my_asset():\n    return B\n");
    assert_ne!(
        cone_hash(&tuple(2), "my_asset"),
        cone_hash(&tuple(3), "my_asset")
    );
    let augmented = |n: u32| format!("A = 1\nA += {n}\n\ndef my_asset():\n    return A\n");
    assert_ne!(
        cone_hash(&augmented(2), "my_asset"),
        cone_hash(&augmented(3), "my_asset")
    );
}

#[test]
fn module_control_flow_bindings_follow_project_helpers() {
    for (bindings, expression) in [
        ("for _ in [0]:\n    VALUE = compute()", "VALUE"),
        (
            "VALUE = 0\nwhile VALUE == 0:\n    VALUE = compute()",
            "VALUE",
        ),
        ("with manager(compute()) as VALUE:\n    pass", "VALUE"),
        ("match compute():\n    case VALUE:\n        pass", "VALUE"),
        (
            "for _ in [0]:\n    def result(): return compute()",
            "result()",
        ),
        (
            "with manager():\n    class Result:\n        value = compute()",
            "Result.value",
        ),
        (
            "for _ in [0]:\n    VALUE = compute()\nCAPTURED = VALUE\nVALUE = 0",
            "CAPTURED",
        ),
    ] {
        let entry = format!(
            "from helpers import compute\n{bindings}\n\ndef my_asset(): return {expression}\n"
        );
        assert_ne!(
            hash_of(&entry, &one("helpers", HELPERS_V1), &[]),
            hash_of(&entry, &one("helpers", HELPERS_V2), &[]),
            "{bindings}"
        );
    }
}

#[test]
fn helper_star_imports_follow_all_sources_and_transitive_exports() {
    for (helper, expression) in [
        (
            "from values import *\ndef result(): return VALUE",
            "helpers.result()",
        ),
        (
            "from values import *\nclass Holder: value = VALUE",
            "helpers.Holder.value",
        ),
        ("from values import *\nCAPTURED = VALUE", "helpers.CAPTURED"),
        ("from values import *", "helpers.VALUE"),
        ("from values import *\nfrom other import *", "helpers.VALUE"),
        (
            "if True:\n    from values import *\ndef result(): return VALUE",
            "helpers.result()",
        ),
        (
            "from middle import *\ndef result(): return VALUE",
            "helpers.result()",
        ),
    ] {
        let entry = format!("import helpers\n\ndef my_asset(): return {expression}\n");
        let modules = |value| {
            sources(&[
                ("helpers", helper),
                ("values", &format!("VALUE = {value}\n")),
                ("other", "OTHER = 7\n"),
                ("middle", "from values import *\n"),
            ])
        };
        assert_ne!(
            hash_of(&entry, &modules(1), &[]),
            hash_of(&entry, &modules(222), &[]),
            "{helper}"
        );
    }
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
            "importlib",
            "import importlib\n\ndef my_asset():\n    return importlib.import_module('helpers').compute()\n",
        ),
        (
            "__import__",
            "def my_asset():\n    return __import__('helpers').compute()\n",
        ),
    ] {
        assert_eq!(
            hash_of(entry, &v1, &[]),
            hash_of(entry, &v2, &[]),
            "{what} is followed now: update `barca docs cache`"
        );
    }
}

/// A module reached through another module's plain `import` (#295): `from wrapper import
/// helpers`, `wrapper.helpers.compute`, a re-export of a re-export, an alias, a local
/// import and the module used as a value all follow `helpers`.
#[test]
fn a_module_reexported_by_a_plain_import_is_followed() {
    let wrappers = [
        ("wrapper", "import helpers\n"),
        ("aliased", "import helpers as h\n"),
        ("outer", "from wrapper import helpers\n"),
        ("relay", "import outer\n"),
    ];
    let with = |helpers: &str| {
        let mut m = one("helpers", helpers);
        for (name, src) in wrappers {
            m.insert(name.into(), src.into());
        }
        m
    };
    for entry in [
        "from wrapper import helpers\n\ndef my_asset():\n    return helpers.compute()\n",
        "import wrapper\n\ndef my_asset():\n    return wrapper.helpers.compute()\n",
        "from aliased import h\n\ndef my_asset():\n    return h.compute()\n",
        "from outer import helpers\n\ndef my_asset():\n    return helpers.compute()\n",
        "import relay\n\ndef my_asset():\n    return relay.outer.helpers.compute()\n",
        "def my_asset():\n    from wrapper import helpers\n    return helpers.compute()\n",
        "from wrapper import helpers\n\ndef my_asset():\n    return getattr(helpers, 'compute')()\n",
    ] {
        assert_ne!(
            hash_of(entry, &with(HELPERS_V1), &[]),
            hash_of(entry, &with(HELPERS_V2), &[]),
            "{entry}"
        );
    }
    // Only the attribute read is followed: another helper in `helpers` stays out.
    let entry = "from wrapper import helpers\n\ndef my_asset():\n    return helpers.compute()\n";
    assert_eq!(
        hash_of(entry, &with(HELPERS_V1), &[]),
        hash_of(entry, &with(HELPERS_UNRELATED_EDIT), &[])
    );
}

/// Re-exports that never reach a module terminate: a module re-exported in a cycle, and a
/// re-exported name that is not a module.
#[test]
fn reexport_cycles_terminate() {
    let project = sources(&[
        ("a", "from b import helpers\n"),
        ("b", "from a import helpers\n"),
        ("c", "from d import f\n"),
        ("d", "from c import f\n"),
    ]);
    for entry in [
        "from a import helpers\n\ndef my_asset():\n    return helpers.compute()\n",
        "import a\n\ndef my_asset():\n    return a.helpers.compute()\n",
        "from c import f\n\ndef my_asset():\n    return f()\n",
    ] {
        assert!(!hash_of(entry, &project, &[]).is_empty(), "{entry}");
    }
}

#[test]
fn a_function_bound_in_module_level_conditional_is_tracked() {
    let entry = "from helpers import compute\n\ndef my_asset():\n    return compute()\n";
    let nested = |n: u32| {
        one(
            "helpers",
            &format!("if True:\n    def compute():\n        return {n}\n"),
        )
    };
    assert_ne!(
        hash_of(entry, &nested(1), &[]),
        hash_of(entry, &nested(2), &[])
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

const STABLE_TWO_MARKERS: &str = "a1755524889bd9195e25225e96c217f3fede7587f0b64dc6be0d5c17b08e638a";

/// Import chains are followed at any depth (#295; up to 0.24 they stopped six modules
/// deep), and modules that import each other terminate.
#[test]
fn import_chains_are_followed_at_any_depth() {
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
    for depth in [6, 7, 10, 200] {
        assert_ne!(
            hash_of(entry, &project(depth, 1), &[]),
            hash_of(entry, &project(depth, 2), &[]),
            "{depth}"
        );
    }
    // A cycle: every module's `f` calls the next one's, and the last calls the first's.
    let ring = |leaf: u32| -> HashMap<String, String> {
        (1..=12)
                .map(|i| {
                    let next = i % 12 + 1;
                    let src = format!(
                        "import m{next}\nfrom m{next} import f as g\n\nN = {}\n\ndef f(n):\n    return N if n == 0 else g(n - 1) + m{next}.N\n",
                        if i == 12 { leaf } else { i }
                    );
                    (format!("m{i}"), src)
                })
                .collect()
    };
    assert_ne!(hash_of(entry, &ring(1), &[]), hash_of(entry, &ring(2), &[]));
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

#[test]
fn foreign_node_decorator_implementations_are_followed() {
    for name in ["asset", "task", "sensor", "unsafe", "sink"] {
        let pipeline = |result: &str| {
            format!(
                "from barca import asset as actual_asset\ndef {name}(**kwargs):\n    return lambda fn: {result}\n\n@actual_asset()\n@{name}()\ndef my_asset():\n    return 0\n"
            )
        };
        let first = cone_hash(&pipeline("1"), "my_asset");
        assert!(!first.is_empty(), "{name}");
        assert_ne!(first, cone_hash(&pipeline("2"), "my_asset"), "{name}");
    }
}

// ─── Rule 7: nodes and their decorators (#283) ───────────────────────────

/// A pipeline file made of `constants` and one node `my_asset` under `decorators`.
fn decorated(constants: &str, decorators: &str) -> String {
    format!(
        "from barca import asset, sensor, sink, partitions, partitions_from, collect, asset_ref\n{constants}\n\n{decorators}\ndef my_asset(x=None):\n    return 1\n"
    )
}

#[test]
fn a_name_in_a_counted_decorator_argument_is_followed() {
    for decorators in [
        "@asset(serializer=FMT)",
        "@asset()\n@sink(PATH_PREFIX + \"out.json\", serializer=FMT)",
        "@asset()\n@retry(times=FMT)",
        "@asset(codec=make(FMT))",
        "@asset(**FMT)",
    ] {
        let hash = |constants: &str| cone_hash(&decorated(constants, decorators), "my_asset");
        let before = hash("FMT = 'json'\nPATH_PREFIX = 'a/'\nOTHER = 1");
        assert!(!before.is_empty(), "{decorators}");
        assert_ne!(
            before,
            hash("FMT = 'pickle'\nPATH_PREFIX = 'a/'\nOTHER = 1"),
            "{decorators}: editing FMT"
        );
        let unrelated = hash("FMT = 'json'\nPATH_PREFIX = 'a/'\nOTHER = 2");
        if decorators.contains("@retry") {
            assert_ne!(before, unrelated, "foreign wrappers count the whole module");
        } else {
            assert_eq!(
                before, unrelated,
                "{decorators}: editing an unused constant"
            );
        }
    }
}

#[test]
fn a_name_in_an_argument_that_does_not_count_is_not_followed() {
    for decorators in [
        "@asset(partitions={\"region\": partitions(REGIONS)})",
        "@asset(partitions={\"region\": partitions([r for r in REGIONS])})",
        "@asset(partitions={\"region\": partitions(regions(REGIONS))})",
        "@asset(description=REGIONS)",
        "@asset(tags=REGIONS, retries=REGIONS, retry_backoff=REGIONS, timeout_seconds=REGIONS)",
        "@asset(freshness=Schedule(REGIONS), name=REGIONS)",
    ] {
        let hash = |constants: &str| cone_hash(&decorated(constants, decorators), "my_asset");
        assert_eq!(hash("REGIONS = ['us', 'eu']"), "", "{decorators}");
        assert_eq!(hash("REGIONS = ['us', 'eu', 'apac']"), "", "{decorators}");
    }
}

#[test]
fn another_decorator_s_definition_is_followed() {
    let pipeline = |wrapper_body: &str| {
        format!(
            "def traced(fn):\n    {wrapper_body}\n\n\n@asset()\n@traced\ndef my_asset():\n    return 1\n"
        )
    };
    assert_ne!(
        cone_hash(&pipeline("return fn"), "my_asset"),
        cone_hash(&pipeline("return lambda: fn() + 1"), "my_asset"),
    );

    // The same through a project module.
    let entry = "from wrappers import traced\n\n\n@asset()\n@traced(level=2)\ndef my_asset():\n    return 1\n";
    let module = |body: &str| {
        one(
            "wrappers",
            &format!("def traced(level):\n    {body}\n\n\ndef other():\n    return 0\n"),
        )
    };
    let before = hash_of(entry, &module("return lambda fn: fn"), &[]);
    assert_ne!(
        before,
        hash_of(
            entry,
            &module("return lambda fn: (lambda: fn() + level)"),
            &[]
        )
    );
    let unrelated = one(
        "wrappers",
        "def traced(level):\n    return lambda fn: fn\n\n\ndef other():\n    return 1\n",
    );
    assert_eq!(before, hash_of(entry, &unrelated, &[]));
}

/// `inputs={"x": up}` names the upstream. The upstream's code already reaches the consumer:
/// its run hash, which covers its definition, is part of the consumer's run hash (for a
/// sensor, next to the hash of its output). Following the reference would put the same
/// code in the consumer's definition hash a second time and change nothing about what
/// re-runs. (An edit to a sensor's code does re-run its consumers, through the run hash,
/// whatever the sensor returns.)
#[test]
fn an_upstream_named_in_inputs_or_partitions_from_is_not_followed() {
    for decorators in [
        "@asset(inputs={\"x\": up})",
        "@asset(inputs={\"x\": collect(up)})",
        "@asset(partitions={\"region\": partitions_from(up)})",
        "@asset(inputs={\"x\": asset_ref(\"pipeline.py:up\")})",
    ] {
        let pipeline = |up_body: &str| {
            format!(
                "from barca import asset, sensor, collect, partitions_from, asset_ref\n@sensor()\ndef up():\n    return {up_body}\n\n\n{decorators}\ndef my_asset(x=None, region=None):\n    return 1\n"
            )
        };
        assert_eq!(cone_hash(&pipeline("1"), "my_asset"), "", "{decorators}");
        assert_eq!(cone_hash(&pipeline("2"), "my_asset"), "", "{decorators}");
    }
}

/// A parameter named after its upstream (`def margin(region, sales)`) counts as a use of the
/// module-level `sales` (rule 2), so `sales` is in `margin`'s cone. It is there as its
/// definition text: adding a key to `sales`, or editing its description, must not re-run
/// `margin`.
#[test]
fn a_node_reached_by_name_contributes_its_definition_not_its_decorator_text() {
    let pipeline = |sales_decorator: &str, revenue: &str| {
        format!(
            "from barca import asset, partitions, partitions_from\n{sales_decorator}\ndef sales(region):\n    return {revenue}\n\n\n@asset(partitions={{\"region\": partitions_from(sales)}})\ndef margin(region, sales):\n    return sales * 0.2\n"
        )
    };
    let hash = |decorator: &str, revenue: &str| cone_hash(&pipeline(decorator, revenue), "margin");
    let before = hash(
        "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})",
        "100",
    );
    assert!(!before.is_empty());
    for same in [
        "@asset(partitions={\"region\": partitions([\"us\", \"eu\", \"apac\"])})",
        "@asset(partitions={\"region\": partitions([\"eu\"])})",
        "@asset(partitions={'region': partitions(['us', 'eu'])}, description=\"sales\")",
        "@asset(\n    partitions={\"region\": partitions([\"us\", \"eu\"])},  # keys\n    retries=3,\n)",
    ] {
        assert_eq!(before, hash(same, "100"), "{same}");
    }
    assert_ne!(
        before,
        hash(
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])})",
            "200"
        ),
        "the upstream's body"
    );
    assert_ne!(
        before,
        hash(
            "@asset(partitions={\"region\": partitions([\"us\", \"eu\"])}, serializer=\"pickle\")",
            "100"
        ),
        "a counted argument of the upstream"
    );
}

/// Rule 8 (#628): a consumer's parameter named after an `@asset(cutoff=True)` does not
/// bring that asset's code into the consumer's cone. Its consumers fold in its bytes, so
/// an edit that writes the same bytes must leave them cached. Without `cutoff`, rule 2.
#[test]
fn a_cutoff_asset_reached_by_name_adds_a_marker_not_its_code() {
    let pipeline = |cutoff: &str, body: &str| {
        format!(
            "from barca import asset\n\ndef helper():\n    return 3\n\n\n@asset({cutoff})\ndef rates():\n    return {body}\n\n\n@asset(inputs={{\"rates\": rates}})\ndef billed(rates):\n    return rates * 2\n"
        )
    };
    let hash = |cutoff: &str, body: &str| cone_hash(&pipeline(cutoff, body), "billed");
    assert_eq!(
        hash("cutoff=True", "1"),
        hash("cutoff=True", "helper() - 2")
    );
    assert!(!hash("cutoff=True", "1").is_empty());
    assert_ne!(hash("", "1"), hash("", "helper() - 2"));
    assert_ne!(hash("cutoff=True", "1"), hash("", "1"));
    assert_eq!(hash("cutoff=False", "1"), hash("", "1"));
    // The cutoff asset's own cone still covers what it uses.
    let own = |body: &str| cone_hash(&pipeline("cutoff=True", body), "rates");
    assert_ne!(own("helper()"), own("1"));
}
