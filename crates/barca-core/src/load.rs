//! Loading a project: from source files to a [`Dag`], with dynamic partitions resolved.
//!
//! Parsing is static (ruff's AST); the only Python that runs is the subprocess that lists a
//! dynamic partition's values.

use crate::BarcaError;
use crate::dag::Dag;
use crate::parse::extract_nodes;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Build the DAG from source files. The work is genuinely blocking (file I/O,
/// parsing, and a Python subprocess for dynamic partitions), so it runs on the
/// blocking pool rather than an async worker thread.
pub async fn build_dag(file_args: &[String], python: &std::path::Path) -> Result<Dag, BarcaError> {
    let files = file_args.to_vec();
    let py = python.to_path_buf();
    tokio::task::spawn_blocking(move || build_dag_blocking(&files, &py))
        .await
        .map_err(|e| BarcaError::Other(format!("DAG analysis task failed: {e}")))?
}

/// A source or definition that could not join the served graph.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LoadError {
    pub file: String,
    pub error: String,
    pub affected_nodes: Vec<String>,
}

/// Serve-only loading: retain valid definitions and report invalid sources/dependents.
pub async fn build_partial_dag(
    files: &[String],
    python: &std::path::Path,
) -> Result<(Dag, Vec<LoadError>), BarcaError> {
    let files = files.to_vec();
    let python = python.to_path_buf();
    tokio::task::spawn_blocking(move || load_blocking(&files, &python, true))
        .await
        .map_err(|e| BarcaError::Other(format!("DAG analysis task failed: {e}")))?
}

/// The directory a pipeline file lives in, as a path that can be read: the directory its helper
/// modules are scanned from (static analysis), and the one `barca serve --watch` watches.
///
/// `Path::new("p.py").parent()` is `Some("")`, an empty path that `read_dir` cannot open, so a
/// bare filename used to scan no helpers at all (#178). Every spelling of the same file must
/// scan the same directory: `p.py` and `./p.py` give `.`, `sub/p.py` gives `sub`, and an
/// absolute path gives its parent. (The CLI already normalizes file arguments to root-relative
/// paths, so node ids do not depend on the spelling either.)
pub fn source_dir(file: &std::path::Path) -> PathBuf {
    match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

pub(crate) fn build_dag_blocking(
    file_args: &[String],
    python: &std::path::Path,
) -> Result<Dag, BarcaError> {
    load_blocking(file_args, python, false).map(|(dag, _)| dag)
}

fn load_blocking(
    file_args: &[String],
    python: &std::path::Path,
    partial: bool,
) -> Result<(Dag, Vec<LoadError>), BarcaError> {
    let mut errors = Vec::new();
    // Parse every file once.
    let mut sources: Vec<(PathBuf, std::rc::Rc<str>)> = Vec::with_capacity(file_args.len());
    let mut nodes_by_file: Vec<Vec<crate::model::ExtractedNode>> = Vec::new();
    for arg in file_args {
        let path = PathBuf::from(arg);
        let parsed = fs::read_to_string(&path)
            .map_err(|e| BarcaError::Usage(format!("{}: {e}", path.display())))
            .and_then(|source| {
                extract_nodes(&source, arg)
                    .map(|nodes| (source, nodes))
                    .map_err(|e| BarcaError::Parse(e.to_string()))
            });
        let (source, nodes) = match parsed {
            Ok(parsed) => parsed,
            Err(error) if partial => {
                errors.push(LoadError {
                    file: arg.clone(),
                    error: error.to_string(),
                    affected_nodes: Vec::new(),
                });
                continue;
            }
            Err(error) => return Err(error),
        };
        nodes_by_file.push(nodes);
        sources.push((path, source.into()));
    }

    // Dependency cones. The project root is the working directory (the CLI changes into it),
    // and the workers' too, so it is where their imports resolve from. Helper modules are read
    // only when a step's cone reaches them, each at most once (`project_modules`).
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut cones = crate::project_modules::ProjectCones::new(
        &root,
        sources
            .iter()
            .map(|(path, source)| (path.as_path(), source.clone())),
    );
    let import_errors = cones.validate_imports(&nodes_by_file);
    if !partial && let Some(error) = import_errors.values().next() {
        return Err(BarcaError::Usage(error.clone()));
    }
    // Nodes in command-line order (the last asset is `get file.py`'s final value).
    let mut all_nodes: Vec<crate::model::ExtractedNode> = Vec::new();
    for (index, nodes) in nodes_by_file.into_iter().enumerate() {
        if partial && !import_errors.is_empty() {
            let path = &sources[index].0;
            let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            if let Some(error) = import_errors.get(&canonical) {
                errors.push(LoadError {
                    file: path.to_string_lossy().into_owned(),
                    error: error.clone(),
                    affected_nodes: nodes.iter().map(|node| node.continuity_key()).collect(),
                });
                continue;
            }
        }
        let pipeline = cones.pipeline(index);
        for mut node in nodes {
            node.cone_hash = pipeline.hash(&node.function_name);
            all_nodes.push(node);
        }
    }

    if !partial {
        crate::dag::validate_partition_dimensions(&all_nodes)?;
        resolve_dynamic_partitions(&mut all_nodes, python);
        return Ok((Dag::build(&all_nodes)?, errors));
    }
    let failed_files = errors.iter().map(|e| e.file.clone()).collect::<Vec<_>>();
    let (dag, mut failures) = Dag::isolate(&all_nodes, &failed_files);
    let mut healthy: Vec<_> = all_nodes
        .iter()
        .filter_map(|node| {
            dag.get_node(&node.continuity_key())
                .map(|node| node.extracted.clone())
        })
        .collect();
    resolve_dynamic_partitions(&mut healthy, python);
    let (dag, additional) = Dag::isolate(&healthy, &failed_files);
    failures.extend(additional);
    for (id, error) in failures {
        let file = all_nodes
            .iter()
            .find(|n| n.continuity_key() == id)
            .map(|n| n.source_file.clone())
            .unwrap_or_default();
        errors.push(LoadError {
            file,
            error,
            affected_nodes: vec![id],
        });
    }
    Ok((dag, errors))
}

fn resolve_dynamic_partitions(nodes: &mut [crate::model::ExtractedNode], python: &std::path::Path) {
    for node in nodes.iter_mut() {
        let mut resolved: Vec<(String, Vec<crate::model::PartitionValue>)> = Vec::new();

        for (dim, spec) in &node.partitions {
            if let crate::model::PartitionSpec::Dynamic { source_text } = spec {
                let module_path = std::path::Path::new(&node.source_file)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(&node.source_file));
                // Compile from source, never a cached .pyc, like the worker (#176).
                let script = "import json, sys\n\
                     from barca._source_import import load_source_module\n\
                     _mod = load_source_module(sys.argv[1], '_m')\n\
                     _ns = vars(_mod); _ns['__builtins__'] = __builtins__\n\
                     print(json.dumps(eval(sys.argv[2], _ns)))\n"
                    .to_string();
                let mut script_file =
                    tempfile::NamedTempFile::new().expect("failed to create temp file");
                use std::io::Write;
                script_file
                    .write_all(script.as_bytes())
                    .expect("failed to write script");
                let script_path = script_file.path().to_path_buf();
                let mut eval = Command::new(python);
                eval.arg(&script_path)
                    .arg(module_path.to_string_lossy().as_ref())
                    .arg(source_text)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
                let output = crate::helper_proc::spawn_std(&mut eval)
                    .and_then(|child| child.wait_with_output())
                    .unwrap_or_else(|e| {
                        panic!(
                            "Failed to evaluate partition expression for {}: {e}",
                            node.function_name
                        )
                    });

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    crate::errln!(
                        "[barca] warning: failed to evaluate partition expression '{}' for {}: {}",
                        source_text,
                        node.function_name,
                        stderr.trim()
                    );
                    continue;
                }

                let stdout = String::from_utf8_lossy(&output.stdout);
                let values: Vec<serde_json::Value> =
                    serde_json::from_str(stdout.trim()).unwrap_or_default();
                let partition_values: Vec<crate::model::PartitionValue> = values
                    .into_iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(crate::model::PartitionValue::Str(s)),
                        serde_json::Value::Number(n) => {
                            n.as_i64().map(crate::model::PartitionValue::Int)
                        }
                        _ => None,
                    })
                    .collect();

                resolved.push((dim.clone(), partition_values));
            }
        }

        for (dim, values) in resolved {
            node.partitions
                .insert(dim, crate::model::PartitionSpec::Static { values });
        }
    }
}

#[cfg(test)]
mod source_dir_tests {
    use super::{build_dag, build_partial_dag, source_dir};
    use std::path::{Path, PathBuf};

    #[tokio::test]
    async fn partial_loading_keeps_colocated_healthy_nodes_without_imports() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.py").display().to_string();
        std::fs::write(&file, "from barca import asset, asset_ref\nraise RuntimeError('must not import')\n@asset(inputs={\"x\": asset_ref('missing.py:bad')})\ndef blocked(x): return x\n@asset(inputs={\"x\": blocked})\ndef dependent(x): return x\n@asset()\ndef healthy(): return 3\n").unwrap();
        assert!(
            build_dag(
                std::slice::from_ref(&file),
                std::path::Path::new("/nonexistent-python")
            )
            .await
            .is_err()
        );
        let (dag, errors) = build_partial_dag(
            std::slice::from_ref(&file),
            std::path::Path::new("/nonexistent-python"),
        )
        .await
        .unwrap();
        assert_eq!(dag.topo_order(), vec![format!("{file}:healthy")]);
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .any(|e| e.affected_nodes == vec![format!("{file}:dependent")])
        );
    }

    #[tokio::test]
    async fn unloaded_source_cannot_redirect_to_another_same_named_node() {
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken.py").display().to_string();
        let valid = dir.path().join("valid.py").display().to_string();
        std::fs::write(&broken, "from barca import asset\ndef broken(: pass").unwrap();
        // Canonical refs are literal; no dynamic concatenation is accepted.
        std::fs::write(&valid, format!("from barca import asset, asset_ref\n@asset()\ndef bad(): return 999\n@asset(inputs={{'x': asset_ref({:?})}})\ndef dependent(x): return x\n", format!("{broken}:bad"))).unwrap();
        let (dag, errors) = build_partial_dag(
            &[broken.clone(), valid.clone()],
            std::path::Path::new("/nonexistent-python"),
        )
        .await
        .unwrap();
        assert_eq!(dag.topo_order(), vec![format!("{valid}:bad")]);
        assert!(
            errors
                .iter()
                .any(|e| e.file == broken && e.affected_nodes.is_empty())
        );
        assert!(errors.iter().any(|e| e.error.contains("unloaded source")
            && e.affected_nodes == vec![format!("{valid}:dependent")]));
    }

    #[test]
    fn partial_reference_guards_preserve_candidate_priority() {
        use crate::{dag::Dag, parse::extract_nodes};
        // Imports prefer siblings; canonical refs prefer their literal path.
        for imported in [true, false] {
            for earlier_healthy in [true, false] {
                let (earlier, later) = if imported {
                    ("sub/shared.py", "shared.py")
                } else {
                    ("shared.py", "sub/shared.py")
                };
                let (healthy, failed) = if earlier_healthy {
                    (earlier, later)
                } else {
                    (later, earlier)
                };
                let mut nodes = extract_nodes(
                    "from barca import asset\n@asset()\ndef value(): return 7\n",
                    healthy,
                )
                .unwrap();
                let declaration = if imported {
                    "from shared import value\n@asset(inputs={'x': value})"
                } else {
                    "@asset(inputs={'x': asset_ref('shared.py:value')})"
                };
                nodes.extend(extract_nodes(&format!("from barca import asset, asset_ref\n{declaration}\ndef consumer(x): return x+1\n"), "sub/p.py").unwrap());
                let (dag, errors) = Dag::isolate(&nodes, &[failed.to_string()]);
                assert_eq!(
                    dag.get_node("sub/p.py:consumer").is_some(),
                    earlier_healthy,
                    "imported={imported}, earlier_healthy={earlier_healthy}: {errors:?}"
                );
                assert_eq!(
                    dag.get_node("sub/p.py:consumer")
                        .map(|n| n.resolved_inputs["x"].as_str()),
                    earlier_healthy
                        .then_some(format!("{healthy}:value"))
                        .as_deref()
                );
            }
        }
    }

    #[tokio::test]
    async fn partial_loading_removes_cycle_and_dependents_but_keeps_neighbour() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("p.py").display().to_string();
        std::fs::write(&file, "from barca import asset, asset_ref\n@asset(inputs={'x': asset_ref('b')})\ndef a(x): return x\n@asset(inputs={'x': a})\ndef b(x): return x\n@asset(inputs={'x': b})\ndef dependent(x): return x\n@asset()\ndef healthy(): return 3\n").unwrap();
        let (dag, errors) = build_partial_dag(
            std::slice::from_ref(&file),
            std::path::Path::new("/nonexistent-python"),
        )
        .await
        .unwrap();
        assert_eq!(dag.topo_order(), vec![format!("{file}:healthy")]);
        assert_eq!(errors.len(), 3);
    }

    #[tokio::test]
    async fn ambiguous_and_duplicate_definitions_do_not_become_valid_after_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.py").display().to_string();
        let b = dir.path().join("b.py").display().to_string();
        let c = dir.path().join("c.py").display().to_string();
        std::fs::write(&a, "from barca import asset, asset_ref\n@asset(inputs={'x': asset_ref('missing')})\ndef named(x): return x\n").unwrap();
        std::fs::write(
            &b,
            "from barca import asset\n@asset()\ndef named(): return 2\n",
        )
        .unwrap();
        std::fs::write(&c, "from barca import asset, asset_ref\n@asset(inputs={'x': asset_ref('named')})\ndef ambiguous(x): return x\n@asset(name='same')\ndef one(): return 1\n@asset(name='same')\ndef two(): return 2\n@asset()\ndef healthy(): return 3\n").unwrap();
        let (dag, errors) = build_partial_dag(
            &[a, b.clone(), c.clone()],
            std::path::Path::new("/nonexistent-python"),
        )
        .await
        .unwrap();
        let ids = dag.topo_order();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&format!("{b}:named").as_str()));
        assert!(ids.contains(&format!("{c}:healthy").as_str()));
        assert!(errors.iter().any(|e| e.error.contains("ambiguous")));
        assert!(
            errors
                .iter()
                .any(|e| e.error.contains("duplicate continuity key"))
        );
    }

    /// Every spelling of a pipeline file names a directory that can be read (#178): a bare
    /// filename used to give an empty path, so no helper module was scanned or hashed.
    #[test]
    fn every_spelling_of_a_file_names_a_readable_directory() {
        assert_eq!(source_dir(Path::new("p.py")), PathBuf::from("."));
        assert_eq!(source_dir(Path::new("./p.py")), PathBuf::from("."));
        assert_eq!(source_dir(Path::new("sub/p.py")), PathBuf::from("sub"));
        assert_eq!(
            source_dir(Path::new("/abs/sub/p.py")),
            PathBuf::from("/abs/sub")
        );
        assert!(std::fs::read_dir(source_dir(Path::new("p.py"))).is_ok());
    }
}
