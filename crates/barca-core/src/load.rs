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
    // Parse every file once.
    let mut sources: Vec<(PathBuf, std::rc::Rc<str>)> = Vec::with_capacity(file_args.len());
    let mut nodes_by_file: Vec<Vec<crate::model::ExtractedNode>> = Vec::new();
    for arg in file_args {
        let path = PathBuf::from(arg);
        let source = fs::read_to_string(&path)
            .map_err(|e| BarcaError::Usage(format!("{}: {e}", path.display())))?;
        nodes_by_file
            .push(extract_nodes(&source, arg).map_err(|e| BarcaError::Parse(e.to_string()))?);
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
    // Nodes in command-line order (the last asset is `get file.py`'s final value).
    let mut all_nodes: Vec<crate::model::ExtractedNode> = Vec::new();
    for (index, nodes) in nodes_by_file.into_iter().enumerate() {
        let pipeline = cones.pipeline(index);
        for mut node in nodes {
            node.cone_hash = pipeline.hash(&node.function_name);
            all_nodes.push(node);
        }
    }

    resolve_dynamic_partitions(&mut all_nodes, python);

    Ok(Dag::build(&all_nodes)?)
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
                    eprintln!(
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
    use super::source_dir;
    use std::path::{Path, PathBuf};

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
