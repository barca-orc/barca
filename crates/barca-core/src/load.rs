//! Loading a project: from source files to a [`Dag`], with dynamic partitions resolved.
//!
//! Parsing is static (ruff's AST); the only Python that runs is the subprocess that lists a
//! dynamic partition's values.

use crate::BarcaError;
use crate::dag::Dag;
use crate::parse::extract_pipeline;
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

/// The pipeline files that the loaded nodes take inputs from but that were not given: the
/// files named in `from pipelines.transforms import daily_revenue` (used in `inputs=` or
/// `partitions_from`) or `asset_ref("pipelines/transforms.py:daily_revenue")`, followed
/// transitively (#551).
///
/// Naming files on the command line selects what is read, but such an input cannot be resolved
/// without reading the file that defines it. Each candidate is the file the DAG resolver would
/// look in (`dag::Resolver::module_files`: a packaged file's absolute import from the root;
/// otherwise its own directory, then the root), kept only when it defines an
/// `@asset`/`@task`/`@sensor` of that name. With the whole project loaded every candidate is
/// already in `loaded`, so nothing is read from disk. Paths are root-relative (the working
/// directory), in the order found.
fn imported_sources(
    loaded_nodes: &[(String, Vec<crate::model::ExtractedNode>)],
    loaded: &std::collections::HashSet<String>,
) -> Vec<String> {
    use crate::model::{NodeRef, PartitionSpec};
    use std::collections::{HashMap, HashSet};
    let norm = |p: &std::path::Path| {
        crate::config::normalize_lexically(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let parse = |file: &str| -> Option<Vec<crate::model::ExtractedNode>> {
        let source = fs::read_to_string(file).ok()?;
        extract_pipeline(&source, file, |_| None)
            .ok()
            .map(|(nodes, _)| nodes)
    };
    let mut loaded: HashSet<String> = loaded.clone();
    // Files probed on disk: their nodes, `None` when missing or unparseable.
    let mut probed: HashMap<String, Option<Vec<crate::model::ExtractedNode>>> = HashMap::new();
    let mut queue: Vec<(String, Vec<crate::model::ExtractedNode>)> = loaded_nodes.to_vec();
    let mut added = Vec::new();
    while let Some((file, nodes)) = queue.pop() {
        for node in &nodes {
            let refs =
                node.inputs
                    .iter()
                    .map(|i| &i.upstream)
                    .chain(node.partitions.values().filter_map(|p| match p {
                        PartitionSpec::DerivedFrom { source_ref } => Some(source_ref),
                        _ => None,
                    }));
            for r in refs {
                let (candidates, name) = match r {
                    NodeRef::Imported { module, name } => (
                        crate::dag::Resolver::module_files(&file, module),
                        name.as_str(),
                    ),
                    NodeRef::Canonical(value) => match value.rsplit_once(':') {
                        Some((path, name)) => (
                            vec![
                                PathBuf::from(path),
                                source_dir(std::path::Path::new(&file)).join(path),
                            ],
                            name,
                        ),
                        None => continue,
                    },
                    NodeRef::FunctionName(_) => continue,
                };
                let candidates: Vec<String> = candidates.iter().map(|c| norm(c)).collect();
                if candidates.iter().any(|c| loaded.contains(c)) {
                    continue;
                }
                let found = candidates.into_iter().find(|c| {
                    probed
                        .entry(c.clone())
                        .or_insert_with(|| {
                            std::path::Path::new(c)
                                .is_file()
                                .then(|| parse(c))
                                .flatten()
                        })
                        .as_ref()
                        .is_some_and(|nodes| nodes.iter().any(|n| n.function_name == name))
                });
                if let Some(found) = found {
                    loaded.insert(found.clone());
                    let nodes = probed.get(&found).cloned().flatten().unwrap_or_default();
                    queue.push((found.clone(), nodes));
                    added.push(found);
                }
            }
        }
    }
    added
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
    load_files(file_args, &[], python, partial)
}

/// [`load_blocking`] of `file_args`, after `imported`: pipeline files read only because a
/// node in `file_args` takes an input from them (#551). Only their nodes upstream of a node in
/// `file_args` are kept, so naming one file never runs another file's unrelated assets.
fn load_files(
    file_args: &[String],
    imported: &[String],
    python: &std::path::Path,
    partial: bool,
) -> Result<(Dag, Vec<LoadError>), BarcaError> {
    let file_args: Vec<String> = imported.iter().chain(file_args).cloned().collect();
    let file_args = file_args.as_slice();
    let mut errors = Vec::new();
    // Read pipeline sources first, so discovery and hashing share the import-path cache.
    let mut sources: Vec<(PathBuf, std::rc::Rc<str>)> = Vec::with_capacity(file_args.len());
    let mut group_declarations = Vec::new();
    let mut nodes_by_file: Vec<Vec<crate::model::ExtractedNode>> = Vec::new();
    for arg in file_args {
        let path = PathBuf::from(arg);
        match fs::read_to_string(&path) {
            Ok(source) => sources.push((path, source.into())),
            Err(error) if partial => errors.push(LoadError {
                file: arg.clone(),
                error: error.to_string(),
                affected_nodes: Vec::new(),
            }),
            Err(error) => return Err(BarcaError::Usage(format!("{}: {error}", path.display()))),
        }
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
    for (index, (path, source)) in sources.iter().enumerate() {
        let file = path.to_string_lossy();
        let pipeline = cones.pipeline(index);
        match extract_pipeline(source, &file, |import| pipeline.star_exports(import)) {
            Ok((nodes, groups)) => {
                nodes_by_file.push(nodes);
                group_declarations.extend(groups);
            }
            Err(error) if partial => {
                errors.push(LoadError {
                    file: file.into_owned(),
                    error: error.to_string(),
                    affected_nodes: Vec::new(),
                });
                nodes_by_file.push(Vec::new());
            }
            Err(error) => return Err(error),
        }
    }
    let import_errors = cones.validate_imports(&nodes_by_file);
    // Project modules that do not parse, by the pipeline file that imports them (#550). The
    // file's steps are refused when a command plans one of them (`Dag::check_imports`).
    let broken_imports: std::collections::HashMap<String, _> = sources
        .iter()
        .enumerate()
        .filter_map(|(index, (path, _))| {
            let broken = cones.broken_import(index)?.clone();
            Some((path.to_string_lossy().into_owned(), broken))
        })
        .collect();
    if !partial && let Some(error) = import_errors.values().next() {
        return Err(BarcaError::Usage(error.clone()));
    }
    // Nodes in command-line order (the last asset is `get file.py`'s final value).
    let mut all_nodes: Vec<crate::model::ExtractedNode> = Vec::new();
    // Each helper's environment reads, parsed once however many steps reach it.
    let mut env_scanned = std::collections::HashMap::new();
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
            let reads;
            (node.cone_hash, node.cone_parts, reads) =
                pipeline.hash_parts_and_env_reads(&node.function_name, &mut env_scanned);
            if !reads.is_empty() {
                let mut all: std::collections::BTreeSet<String> =
                    std::mem::take(&mut node.env_reads).into_iter().collect();
                all.extend(reads);
                node.env_reads = all.into_iter().collect();
            }
            all_nodes.push(node);
        }
    }
    // Every step's cone has been hashed: these are all the project files it looked at.
    let files_read = cones.paths_probed();

    if !partial && imported.is_empty() {
        let loaded_nodes: Vec<(String, Vec<crate::model::ExtractedNode>)> = sources
            .iter()
            .map(|(path, _)| {
                let file = path.to_string_lossy();
                let nodes = all_nodes
                    .iter()
                    .filter(|n| n.source_file == file)
                    .cloned()
                    .collect();
                (file.into_owned(), nodes)
            })
            .collect();
        let loaded = sources
            .iter()
            .map(|(path, _)| {
                crate::config::normalize_lexically(path)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        let more = imported_sources(&loaded_nodes, &loaded);
        if !more.is_empty() {
            // Dependencies first, so the given files' nodes stay last.
            let more: Vec<String> = more.into_iter().rev().collect();
            return load_files(file_args, &more, python, partial);
        }
    }
    if !imported.is_empty() {
        let given: Vec<String> = all_nodes
            .iter()
            .filter(|n| !imported.contains(&n.source_file))
            .map(|n| n.continuity_key())
            .collect();
        let given: Vec<&str> = given.iter().map(String::as_str).collect();
        // `isolate` rather than `build`: a broken definition elsewhere in an imported file must
        // not fail a command that does not use it. The given files' nodes are always kept, so
        // their own errors are reported below.
        let keep: std::collections::HashSet<String> = Dag::isolate(&all_nodes, &[])
            .0
            .subgraph_many(&given)
            .into_iter()
            .map(str::to_string)
            .collect();
        all_nodes
            .retain(|n| !imported.contains(&n.source_file) || keep.contains(&n.continuity_key()));
        // Groups are declared for the files named; an imported file's would name pruned nodes.
        group_declarations.retain(|d| !imported.contains(&d.file));
    }

    if !partial {
        crate::dag::validate_partition_dimensions(&all_nodes)?;
        // Like a file that does not parse: the command cannot plan with a node whose keys are
        // unknown, so it stops with the node named (exit 2).
        let unevaluated = resolve_dynamic_partitions(&mut all_nodes, python)?;
        if !unevaluated.is_empty() {
            let detail: Vec<String> = unevaluated
                .iter()
                .map(|(node, error)| format!("{node}: {error}"))
                .collect();
            return Err(BarcaError::Usage(format!(
                "could not load the partition keys of {}\nThe keys are evaluated when barca \
                 plans, importing the module as a worker does (the file's directory and the \
                 project root on sys.path): fix the error, or list the keys literally or with \
                 partitions_from(...) (barca docs partitions).",
                detail.join("\n")
            )));
        }
        let groups = crate::groups::resolve(group_declarations, &all_nodes)?;
        let mut dag = Dag::build(&all_nodes)?;
        dag.groups = groups;
        dag.broken_imports = broken_imports;
        dag.files_read = files_read;
        return Ok((dag, errors));
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
    let unevaluated = resolve_dynamic_partitions(&mut healthy, python)?;
    let (mut dag, additional) = Dag::isolate_with(&healthy, &failed_files, unevaluated);
    dag.broken_imports = broken_imports;
    dag.files_read = files_read;
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
    let retained: Vec<_> = healthy
        .into_iter()
        .filter(|node| dag.get_node(&node.continuity_key()).is_some())
        .collect();
    // Groups decorate only the retained definitions. An invalid hierarchy must not
    // suppress healthy execution or unrelated files' organizational metadata.
    let mut declarations_by_file = std::collections::BTreeMap::<String, Vec<_>>::new();
    for declaration in group_declarations {
        declarations_by_file
            .entry(declaration.file.clone())
            .or_default()
            .push(declaration);
    }
    let mut owners = std::collections::HashSet::new();
    for (file, declarations) in declarations_by_file {
        match crate::groups::resolve(declarations, &retained) {
            Ok(groups) => {
                if groups
                    .iter()
                    .flat_map(|g| &g.members)
                    .any(|member| owners.contains(member))
                {
                    errors.push(LoadError {
                        file,
                        error: "group members already belong to a group in another source file"
                            .into(),
                        affected_nodes: Vec::new(),
                    });
                } else {
                    owners.extend(groups.iter().flat_map(|g| g.members.iter().cloned()));
                    dag.groups.extend(groups);
                }
            }
            Err(error) => errors.push(LoadError {
                file,
                error: error.to_string(),
                affected_nodes: Vec::new(),
            }),
        }
    }
    Ok((dag, errors))
}

/// A Python traceback on stderr as (its last line, the exception; the frames of the user's
/// code, without barca's loader, importlib and the `-c` script).
fn user_traceback(stderr: &str) -> (String, String) {
    let lines: Vec<&str> = stderr.trim_end().lines().collect();
    let exception = lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .map_or_else(|| "an error".to_string(), |l| l.trim().to_string());
    let mut frames = String::new();
    let mut keep = false;
    for line in &lines {
        if let Some(rest) = line.strip_prefix("  File ") {
            keep = !(rest.starts_with("\"<")
                || rest.contains("/barca/_source_import.py")
                || rest.contains("/importlib/"));
        } else if !line.starts_with(' ') {
            keep = false;
        }
        if keep {
            frames.push_str(line);
            frames.push('\n');
        }
    }
    (exception, frames.trim_end().to_string())
}

/// Evaluate each `partitions(<expression>)` in a Python subprocess, which imports the node's
/// module the way a worker does (`load_pipeline_module`: the file's directory and the project
/// root, the cwd, on `sys.path`, so sibling modules and project packages import). A node whose
/// expression cannot be evaluated is returned as (node id, error) for a load error naming it,
/// with the traceback (#628), instead of being left with no keys.
fn resolve_dynamic_partitions(
    nodes: &mut [crate::model::ExtractedNode],
    python: &std::path::Path,
) -> Result<Vec<(String, String)>, BarcaError> {
    // `-c`: the cwd (the project root) is first on sys.path, as for a worker.
    const SCRIPT: &str = "import json, sys\n\
        from barca._source_import import load_pipeline_module\n\
        _ns = dict(vars(load_pipeline_module(sys.argv[1])))\n\
        _ns['__builtins__'] = __builtins__\n\
        print(json.dumps(list(eval(sys.argv[2], _ns))))\n";
    let mut failures = Vec::new();
    for node in nodes.iter_mut() {
        let mut resolved: Vec<(String, Vec<crate::model::PartitionValue>)> = Vec::new();
        let mut failed = None;
        for (dim, spec) in &node.partitions {
            let crate::model::PartitionSpec::Dynamic { source_text } = spec else {
                continue;
            };
            let module_path = std::path::Path::new(&node.source_file)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(&node.source_file));
            let mut eval = Command::new(python);
            eval.arg("-c")
                .arg(SCRIPT)
                .arg(module_path.to_string_lossy().as_ref())
                .arg(source_text)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let output = match crate::helper_proc::spawn_std(&mut eval)
                .and_then(|child| child.wait_with_output())
            {
                Ok(output) => output,
                // Not the pipeline's fault: barca's own Python is missing (exit 3).
                Err(e) => {
                    return Err(BarcaError::Other(format!(
                        "cannot evaluate partitions({source_text}) of {}: cannot run {}: {e}",
                        node.continuity_key(),
                        python.display()
                    )));
                }
            };
            if !output.status.success() {
                let (exception, frames) = user_traceback(&String::from_utf8_lossy(&output.stderr));
                let mut error =
                    format!("partitions({source_text}) for dimension '{dim}' raised {exception}");
                if !frames.is_empty() {
                    error.push_str("\nTraceback (most recent call last):\n");
                    error.push_str(&frames);
                }
                failed = Some(error);
                break;
            }
            let stdout = String::from_utf8_lossy(&output.stdout);
            let values: Vec<serde_json::Value> = match serde_json::from_str(stdout.trim()) {
                Ok(values) => values,
                Err(e) => {
                    failed = Some(format!(
                        "partitions({source_text}) for dimension '{dim}' did not give a list of \
                         keys: {e}"
                    ));
                    break;
                }
            };
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
        if let Some(error) = failed {
            failures.push((node.continuity_key(), error));
            continue;
        }
        for (dim, values) in resolved {
            node.partitions
                .insert(dim, crate::model::PartitionSpec::Static { values });
        }
        // `batches(<expression>)` and `batches_by(<function>)` (RFC-0013): evaluated with the
        // project importable, like the keys above; an error is a load error naming the node.
        let mut dims: Vec<String> = node.partitions.keys().cloned().collect();
        dims.sort();
        match node.batch.clone() {
            Some(crate::model::BatchSpec::GroupsExpr {
                source_text,
                strict,
            }) => {
                match crate::batch::evaluate_groups(python, &node.source_file, &source_text, &dims)
                {
                    Ok(groups) => {
                        node.batch = Some(crate::model::BatchSpec::Groups { groups, strict });
                    }
                    Err(error) => failures.push((node.continuity_key(), error)),
                }
            }
            Some(crate::model::BatchSpec::By { source_text, .. }) => {
                if let Err(error) =
                    crate::batch::check_label_function(python, &node.source_file, &source_text)
                {
                    failures.push((node.continuity_key(), error));
                }
            }
            _ => {}
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod source_dir_tests {
    use super::{build_dag, build_partial_dag, source_dir};
    use std::path::{Path, PathBuf};

    /// #574: the environment variables a step reads, in its function and in the project
    /// helpers it reaches, are found statically; the undeclared ones are plan warnings (one item
    /// per step and variable, one stderr line per command), and no hash changes.
    #[tokio::test]
    async fn undeclared_env_reads_are_found_through_helpers_and_summarized_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.py"),
            "import os\n\nclass Settings:\n    ROOT = os.environ.get(\"APP_ROOT\", \"data\")\n\n\
             def registry(name):\n    return os.environ.get(f\"{name.upper()}_REGISTRY_CSV\")\n\n\
             def unused():\n    return os.environ[\"NEVER_REACHED\"]\n",
        )
        .unwrap();
        let file = dir.path().join("p.py").display().to_string();
        let source = "import os\nfrom barca import asset\nfrom settings import Settings, registry\n\n\
                      @asset(env=[\"ACCOUNT_REGISTRY_CSV\"])\ndef reg():\n    return [Settings.ROOT, registry(\"account\")]\n\n\
                      @asset(inputs={\"r\": reg})\ndef plain(r):\n    return r, os.getenv(\"TOKEN_A\"), os.environ[\"TOKEN_B\"]\n\n\
                      @asset()\ndef quiet():\n    return 1\n";
        std::fs::write(&file, source).unwrap();
        let dag = build_dag(
            std::slice::from_ref(&file),
            std::path::Path::new("/nonexistent-python"),
        )
        .await
        .unwrap();
        let node = |name: &str| dag.get_node(&format!("{file}:{name}")).unwrap();
        assert_eq!(
            node("reg").extracted.env_reads,
            ["*_REGISTRY_CSV", "APP_ROOT"]
        );
        // The pattern is covered by the declared name that matches it.
        assert_eq!(node("reg").extracted.undeclared_env(), ["APP_ROOT"]);
        assert_eq!(
            node("plain").extracted.undeclared_env(),
            ["TOKEN_A", "TOKEN_B"]
        );
        assert!(node("quiet").extracted.env_reads.is_empty());

        let plan = crate::planner::plan_from_dag(
            &dag,
            &crate::planner::ResourceConfig {
                pool_size: 2,
                concurrency_groups: Default::default(),
            },
        );
        let warnings = crate::warnings::for_plan(&dag, &plan);
        let items: Vec<(&str, &str, &str)> = warnings
            .iter()
            .map(|w| (w.kind.as_str(), &w.node[file.len() + 1..], w.param.as_str()))
            .collect();
        assert_eq!(
            items,
            [
                ("undeclared_env", "plain", "TOKEN_A"),
                ("undeclared_env", "plain", "TOKEN_B"),
                ("undeclared_env", "reg", "APP_ROOT"),
            ]
        );
        // One line per step: every item of a step carries the same message.
        assert_eq!(warnings[0].message, warnings[1].message);
        assert!(warnings[0].message.contains("TOKEN_A, TOKEN_B"));
        assert!(warnings[0].message.contains("env=[...]"));
        // stderr: one summary line for the command.
        assert_eq!(
            crate::warnings::undeclared_env_summary(&warnings).unwrap(),
            "2 steps read environment variables not declared in env=: APP_ROOT, TOKEN_A, \
             TOKEN_B; barca status lists them"
        );
    }

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
