//! No pipeline in this repository trips the unused-input warning by accident (#231).
//!
//! The warning is a static guess about user code, so its worst failure is a false positive on
//! code that is fine. This test runs the check over every pipeline the repository holds:
//!
//! - every `.py` file under `examples/`, `benchmarks/` and `python/` (the files themselves),
//! - every string literal in those files that is itself a pipeline (the fixtures the Python
//!   tests write to disk),
//! - every fenced code block in the manual (`crates/barca-cli/docs`), the site docs, the README
//!   and `SKILL.md`, and every here-document in `tests/integration/*.sh`.
//!
//! and requires the findings to be exactly [`DELIBERATE`]: the places that show or test the
//! warning on purpose. A new entry there needs a reason; anything else is a false positive (fix
//! the rule) or a real unused input (fix the pipeline: use it, remove it, or `_`-prefix it).
//!
//! The same sweep checks decorator arguments (#284): a pipeline that passes a decorator an
//! argument barca does not define is rejected when it is planned, so none in the repository
//! may, except the ones in [`REJECTED_ARGUMENTS`] that show or test that error.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use barca_core::NodeKind;
use barca_core::parse::{ParseError, extract_nodes};
use ruff_python_ast::Expr;
use ruff_python_ast::visitor::{self, Visitor};

/// (file relative to the repository root, function, parameter) that the check must report.
const DELIBERATE: &[(&str, &str, &str)] = &[
    // The manual's own example of the warning, and the site page's "common mistake".
    ("crates/barca-cli/docs/assets.md", "report", "raw"),
    (
        "site/src/content/docs/patterns/03-ordering-only-deps.md",
        "seed_data",
        "migrate",
    ),
    // The CLI contract fixture: one unused input, so the schemas show a filled `warnings`.
    ("python/tests/test_cli_contract.py", "report", "rows"),
    // The tests of the warning itself.
    // ... including the four that mention the input only where it does not count (docstring,
    // comment, a longer identifier, another name), next to the SQL-in-a-string steps that
    // must stay silent.
    // A call named like a query entry point, on a package known to be unrelated.
    (
        "python/tests/test_unused_input_warning.py",
        "arrow_table",
        "orders",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "docstring_only",
        "orders",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "comment_only",
        "orders",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "longer_name",
        "orders",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "another_name",
        "orders",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "report",
        "other",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "dropped",
        "raw",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "lazy_polars",
        "raw",
    ),
    (
        "python/tests/test_unused_input_warning.py",
        "per_key",
        "raw",
    ),
    ("python/tests/test_unused_input_warning.py", "fan", "raw"),
    // #300's shadowing regressions include a genuinely unused input as a control.
    (
        "python/tests/test_unused_input_warning.py",
        "really_unused",
        "orders",
    ),
    ("python/tests/test_warning_dedupe.py", "fetch", "seed"),
];

/// (file, function, argument) of the pipelines that must be rejected because a decorator is
/// called with an argument it does not define (#284): the tests and the documentation of that
/// check. Every other pipeline in the repository must plan.
const REJECTED_ARGUMENTS: &[(&str, &str, &str)] = &[
    // The test that names in other scopes do not turn the check off.
    (
        "python/tests/test_decorator_names_not_barcas.py",
        "t",
        "when",
    ),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn files_under(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !name.starts_with('.') && !["node_modules", "target", "dist"].contains(&&*name) {
                files_under(&path, extensions, out);
            }
        } else if extensions
            .iter()
            .any(|e| path.extension().is_some_and(|x| x == *e))
        {
            out.push(path);
        }
    }
}

/// Every string literal in a Python file.
fn string_literals(source: &str) -> Vec<String> {
    struct Strings(Vec<String>);
    impl<'a> Visitor<'a> for Strings {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::StringLiteral(s) = expr {
                self.0.push(s.value.to_string());
            }
            visitor::walk_expr(self, expr);
        }
    }
    let Ok(parsed) = ruff_python_parser::parse_module(source) else {
        return Vec::new();
    };
    let mut strings = Strings(Vec::new());
    visitor::walk_body(&mut strings, &parsed.syntax().body);
    strings.0
}

/// The bodies of fenced code blocks (markdown) or here-documents (shell): the text between a
/// line that opens one and the line that closes it.
fn embedded_blocks(text: &str, shell: bool) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<(String, String)> = None; // (closing line, body)
    for line in text.lines() {
        match &mut current {
            Some((close, body)) => {
                if line.trim() == close.as_str() {
                    blocks.push(std::mem::take(body));
                    current = None;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
            None if shell => {
                if let Some((_, tag)) = line.split_once("<<") {
                    let tag = tag.trim_start_matches('-').trim();
                    let tag = tag.split_whitespace().next().unwrap_or("");
                    let tag = tag.trim_matches(|c| c == '\'' || c == '"');
                    if !tag.is_empty() && tag.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        current = Some((tag.to_string(), String::new()));
                    }
                }
            }
            None => {
                if line.trim_start().starts_with("```") {
                    current = Some(("```".to_string(), String::new()));
                }
            }
        }
    }
    blocks
}

/// Markdown indents blocks inside lists; strip the common indentation so they parse.
fn dedent(code: &str) -> String {
    let indent = code
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    code.lines()
        .map(|l| l.get(indent..).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Default)]
struct Sweep {
    findings: BTreeSet<(String, String, String)>,
    /// (file, function, argument) of every pipeline rejected for a decorator argument.
    rejected: BTreeSet<(String, String, String)>,
    /// Pipelines checked (sources with at least one decorated function).
    pipelines: usize,
    /// Decorated functions that declare inputs.
    steps_with_inputs: usize,
}

impl Sweep {
    /// Only whether the source is rejected for a decorator argument.
    fn check_arguments(&mut self, file: &str, source: &str) {
        if let Err(ParseError::InvalidArguments {
            function, message, ..
        }) = extract_nodes(source, file)
        {
            let argument = message.split('`').nth(1).unwrap_or("").to_string();
            self.rejected.insert((file.to_string(), function, argument));
        }
    }

    fn check(&mut self, file: &str, source: &str) {
        let nodes = match extract_nodes(source, file) {
            Ok(nodes) => nodes,
            // A pipeline barca refuses to plan because of a decorator argument (#284).
            Err(ParseError::InvalidArguments {
                function, message, ..
            }) => {
                self.pipelines += 1;
                let argument = message.split('`').nth(1).unwrap_or("").to_string();
                self.rejected.insert((file.to_string(), function, argument));
                return;
            }
            // Not Python (a shell block, a template with placeholders).
            Err(_) => return,
        };
        if nodes.is_empty() {
            return;
        }
        self.pipelines += 1;
        // An unused sensor input is a cache trigger, not a finding (`warnings::for_plan` asks
        // the DAG; here the sensors of the same source are enough).
        let sensors: Vec<String> = nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Sensor)
            .map(|n| n.function_name.clone())
            .collect();
        for node in nodes {
            if !node.inputs.is_empty() {
                self.steps_with_inputs += 1;
            }
            for param in node.unused_inputs {
                let upstream = node.inputs.iter().find(|i| i.param_name == param);
                if upstream
                    .is_some_and(|i| sensors.iter().any(|s| s == i.upstream.resolution_name()))
                {
                    continue;
                }
                self.findings
                    .insert((file.to_string(), node.function_name.clone(), param));
            }
        }
    }
}

#[test]
fn only_the_deliberate_examples_trip_the_unused_input_warning() {
    let root = repo_root();
    let rel = |p: &Path| {
        p.strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut sweep = Sweep::default();

    let mut python = Vec::new();
    for dir in ["examples", "benchmarks", "python"] {
        files_under(&root.join(dir), &["py"], &mut python);
    }
    assert!(python.len() > 100, "found only {} .py files", python.len());
    for path in &python {
        let source = std::fs::read_to_string(path).unwrap();
        sweep.check(&rel(path), &source);
        for literal in string_literals(&source) {
            if literal.contains("barca") {
                sweep.check(&rel(path), &literal);
                // Fixtures written indented inside a test and dedented when they are saved:
                // looked at for decorator arguments only. (The unused-input sweep has never
                // read them; two of them have an unused input.)
                let dedented = dedent(&literal);
                if dedented != literal {
                    sweep.check_arguments(&rel(path), &dedented);
                }
            }
        }
    }

    let mut docs = vec![root.join("README.md"), root.join("SKILL.md")];
    files_under(&root.join("crates/barca-cli/docs"), &["md"], &mut docs);
    files_under(&root.join("site/src/content"), &["md", "mdx"], &mut docs);
    for path in &docs {
        let text = std::fs::read_to_string(path).unwrap();
        for block in embedded_blocks(&text, false) {
            sweep.check(&rel(path), &dedent(&block));
        }
    }

    let mut scripts = Vec::new();
    files_under(&root.join("tests/integration"), &["sh"], &mut scripts);
    files_under(&root.join("benchmarks"), &["sh"], &mut scripts);
    for path in &scripts {
        let text = std::fs::read_to_string(path).unwrap();
        for block in embedded_blocks(&text, true) {
            sweep.check(&rel(path), &block);
        }
    }

    // The sweep really looked at the repository's pipelines.
    assert!(sweep.pipelines > 300, "only {} pipelines", sweep.pipelines);
    assert!(
        sweep.steps_with_inputs > 500,
        "only {} steps with inputs",
        sweep.steps_with_inputs
    );

    let rejected: BTreeSet<(String, String, String)> = REJECTED_ARGUMENTS
        .iter()
        .map(|(f, n, a)| (f.to_string(), n.to_string(), a.to_string()))
        .collect();
    assert!(
        sweep.rejected == rejected,
        "pipelines rejected for a decorator argument differ from REJECTED_ARGUMENTS.\n\
         Not listed (remove or correct the argument, or list it if it shows the check): {:#?}\n\
         Listed but no longer rejected: {:#?}",
        sweep.rejected.difference(&rejected).collect::<Vec<_>>(),
        rejected.difference(&sweep.rejected).collect::<Vec<_>>()
    );

    let expected: BTreeSet<(String, String, String)> = DELIBERATE
        .iter()
        .map(|(f, n, p)| (f.to_string(), n.to_string(), p.to_string()))
        .collect();
    let unexpected: Vec<_> = sweep.findings.difference(&expected).collect();
    let missing: Vec<_> = expected.difference(&sweep.findings).collect();
    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "unused-input findings differ from DELIBERATE.\n\
         Reported but not expected (a false positive, or a real unused input to fix):\n{unexpected:#?}\n\
         Expected but not reported:\n{missing:#?}\n\
         ({} pipelines, {} steps with inputs checked)",
        sweep.pipelines,
        sweep.steps_with_inputs
    );
}

#[test]
fn the_sweep_helpers_find_embedded_pipelines() {
    let md =
        "text\n```python\nfrom barca import asset\n```\n1. item\n   ```python\n   x = 1\n   ```\n";
    let blocks = embedded_blocks(md, false);
    assert_eq!(blocks.len(), 2);
    assert_eq!(dedent(&blocks[1]), "x = 1");
    let sh =
        "cat > p.py <<'EOF'\nfrom barca import asset\nEOF\necho hi\ncat <<PY > q.py\ny = 2\nPY\n";
    assert_eq!(
        embedded_blocks(sh, true),
        ["from barca import asset\n", "y = 2\n"]
    );
    assert_eq!(
        string_literals("A = '''x = 1'''\nB = f(\"y\")"),
        ["x = 1", "y"]
    );
}
