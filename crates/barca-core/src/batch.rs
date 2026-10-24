//! Batched partitions (RFC-0013): `batch=` groups the keys of a partitioned `@asset` or
//! `@task` that need to run into fewer calls of its function.
//!
//! Batching is scheduling only. Each key keeps the run hash, artifact and materialization it
//! has without batching; the planner decides which keys run exactly as before, and the keys of
//! a node that run in a phase are then grouped into calls ([`group_keys`]). The coordinator
//! starts a call once every key of it is ready, and leases its keys to one worker together
//! (`crate::coordinator`); the worker calls the function once and reports each key on its
//! own (`python/barca/_worker.py`).
//!
//! This module holds what is shared: reading `batch=` from the source ([`extract`]), the
//! per-run override (`--batch`, [`BatchOverride`]), the grouping, and the Python evaluation
//! of `batches(<expression>)` and `batches_by(<function>)`.

use ruff_python_ast::{self as ast, Expr};
use ruff_text_size::Ranged;

use crate::model::BatchSpec;

// ─── Reading `batch=` ────────────────────────────────────────────────────────

/// The fix shown with every invalid `batch=`.
pub const FIX: &str = "write batch=N (a whole number of at least 1), batch=\"all\", \
                       batch=batches([[...], ...]) or batch=batches_by(fn, max_size=N) on a \
                       partitioned @asset or @task (see `barca docs partitions`, \"Processing \
                       many keys per call\")";

fn text(source: &str, expr: &Expr) -> String {
    let r = expr.range();
    source
        .get(r.start().to_usize()..r.end().to_usize())
        .unwrap_or("?")
        .to_string()
}

fn literal_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::StringLiteral(s) => Some(s.value.to_string()),
        Expr::NumberLiteral(n) => match &n.value {
            ast::Number::Int(i) => i.as_i64().map(|i| i.to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// One member of a literal `batches([...])` group, as the key's inner display form
/// (`retailer=r1`). A single-dimension node names keys by value; a node of several
/// dimensions by a dict of dimension to value.
fn member(expr: &Expr, dims: &[String], source: &str) -> Result<String, (usize, String)> {
    let at = expr.range().start().to_usize();
    if let Some(value) = literal_key(expr) {
        if dims.len() == 1 {
            return Ok(format!("{}={value}", dims[0]));
        }
        return Err((
            at,
            format!(
                "batches(...) names the key {} of a node with the dimensions {}; name each key \
                 as a dict of dimension to value, e.g. {{\"{}\": ...}}",
                text(source, expr),
                dims.join(", "),
                dims[0]
            ),
        ));
    }
    if let Expr::Dict(dict) = expr {
        let mut pairs = std::collections::BTreeMap::new();
        for item in &dict.items {
            let (Some(k), Some(v)) = (
                item.key.as_ref().and_then(literal_key),
                literal_key(&item.value),
            ) else {
                return Err((
                    at,
                    format!(
                        "batches(...) member {} is not a dict of literal dimension names to keys",
                        text(source, expr)
                    ),
                ));
            };
            pairs.insert(k, v);
        }
        let mut names: Vec<&String> = pairs.keys().collect();
        names.sort();
        let mut want: Vec<&String> = dims.iter().collect();
        want.sort();
        if names != want {
            return Err((
                at,
                format!(
                    "batches(...) member {} does not name every dimension of the node ({})",
                    text(source, expr),
                    dims.join(", ")
                ),
            ));
        }
        return Ok(pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(","));
    }
    Err((
        at,
        format!(
            "batches(...) member {} is not a key literal",
            text(source, expr)
        ),
    ))
}

fn is_literal(expr: &Expr) -> bool {
    match expr {
        Expr::StringLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_) => true,
        Expr::Dict(d) => d
            .items
            .iter()
            .all(|item| item.key.as_ref().is_some_and(is_literal) && is_literal(&item.value)),
        Expr::List(l) => l.elts.iter().all(is_literal),
        Expr::Tuple(t) => t.elts.iter().all(is_literal),
        Expr::UnaryOp(u) => is_literal(&u.operand),
        _ => false,
    }
}

/// The groups of a literal `batches([[...], ...])`, checked: no empty group, no key in two.
fn literal_groups(
    list: &ast::ExprList,
    dims: &[String],
    source: &str,
) -> Result<Option<Vec<Vec<String>>>, (usize, String)> {
    let mut groups = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for group in &list.elts {
        let Expr::List(members) = group else {
            // Not a literal list of lists: evaluated by Python like any other expression.
            return Ok(None);
        };
        if members.elts.is_empty() {
            return Err((
                group.range().start().to_usize(),
                "batches(...) has an empty group; every group names at least one key".to_string(),
            ));
        }
        let mut keys = Vec::new();
        for m in &members.elts {
            if !is_literal(m) {
                return Ok(None);
            }
            let key = member(m, dims, source)?;
            if !seen.insert(key.clone()) {
                return Err((
                    m.range().start().to_usize(),
                    format!("batches(...) names the key {key} in two groups"),
                ));
            }
            keys.push(key);
        }
        groups.push(keys);
    }
    Ok(Some(groups))
}

fn positive_int(expr: &Expr) -> Option<i128> {
    match expr {
        Expr::NumberLiteral(n) => match &n.value {
            ast::Number::Int(i) => i.as_i64().map(i128::from),
            _ => None,
        },
        Expr::UnaryOp(u) if u.op == ast::UnaryOp::USub => positive_int(&u.operand).map(|n| -n),
        _ => None,
    }
}

/// `batch=` as written: `None` when not written or `batch=None`. `dims` are the node's
/// partition dimensions (sorted). An invalid value is `Err((offset, message))`.
pub(crate) fn extract(
    value: &Expr,
    source: &str,
    barca: &crate::decorator_args::BarcaNames,
    dims: &[String],
) -> Result<Option<BatchSpec>, (usize, String)> {
    let at = value.range().start().to_usize();
    let bad = |what: String| Err((at, what));
    match value {
        Expr::NoneLiteral(_) => Ok(None),
        Expr::StringLiteral(s) if s.value.to_str() == "all" => Ok(Some(BatchSpec::All)),
        Expr::StringLiteral(_) => bad(format!(
            "batch= must be a whole number, \"all\", batches(...) or batches_by(...), got {}",
            text(source, value)
        )),
        Expr::NumberLiteral(_) | Expr::UnaryOp(_) => match positive_int(value) {
            Some(n) if n >= 1 && n <= i128::from(u32::MAX) => {
                Ok(Some(BatchSpec::Size { size: n as u32 }))
            }
            _ => bad(format!(
                "batch= must be a whole number of at least 1, got {}",
                text(source, value)
            )),
        },
        Expr::Call(call) => match barca.recognized(&call.func) {
            Some("batches") => {
                let strict = match call
                    .arguments
                    .keywords
                    .iter()
                    .find(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == "strict"))
                {
                    None => false,
                    Some(k) => match &k.value {
                        Expr::BooleanLiteral(b) => b.value,
                        other => {
                            return Err((
                                other.range().start().to_usize(),
                                format!(
                                    "batches(..., strict=) must be True or False, got {}",
                                    text(source, other)
                                ),
                            ));
                        }
                    },
                };
                let Some(arg) = call.arguments.args.first() else {
                    return bad("batches() needs the list of groups".to_string());
                };
                if let Expr::List(list) = arg
                    && let Some(groups) = literal_groups(list, dims, source)?
                {
                    if groups.is_empty() {
                        return bad("batches([]) names no group".to_string());
                    }
                    return Ok(Some(BatchSpec::Groups { groups, strict }));
                }
                Ok(Some(BatchSpec::GroupsExpr {
                    source_text: text(source, arg),
                    strict,
                }))
            }
            Some("batches_by") => {
                let Some(arg) = call.arguments.args.first() else {
                    return bad("batches_by() needs the function that labels a key".to_string());
                };
                let max_size = match call
                    .arguments
                    .keywords
                    .iter()
                    .find(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == "max_size"))
                {
                    None => None,
                    Some(k) => match &k.value {
                        Expr::NoneLiteral(_) => None,
                        v => match positive_int(v) {
                            Some(n) if n >= 1 && n <= i128::from(u32::MAX) => Some(n as u32),
                            _ => {
                                return Err((
                                    v.range().start().to_usize(),
                                    format!(
                                        "batches_by(..., max_size=) must be a whole number of at \
                                         least 1, got {}",
                                        text(source, v)
                                    ),
                                ));
                            }
                        },
                    },
                };
                Ok(Some(BatchSpec::By {
                    source_text: text(source, arg),
                    max_size,
                }))
            }
            _ => bad(format!(
                "batch= must be a whole number, \"all\", batches(...) or batches_by(...), got {}",
                text(source, value)
            )),
        },
        _ => bad(format!(
            "batch= must be a whole number, \"all\", batches(...) or batches_by(...), got {}",
            text(source, value)
        )),
    }
}

/// Whether an annotation on the partition parameter of a batched function is a scalar
/// (`str`, `int`, ...): the function receives a list of keys, so that is an error.
pub(crate) fn scalar_annotation(annotation: &Expr) -> Option<String> {
    let name = match annotation {
        Expr::Name(n) => n.id.to_string(),
        Expr::StringLiteral(s) => s.value.to_string(),
        _ => return None,
    };
    matches!(name.as_str(), "str" | "int" | "float" | "bytes" | "bool").then_some(name)
}

// ─── The per-run override ───────────────────────────────────────────────────

/// One `--batch` value: one key per call, at most N, or all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverrideValue {
    One,
    Size(u32),
    All,
}

impl OverrideValue {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim() {
            "none" | "None" | "1" => Ok(OverrideValue::One),
            "all" => Ok(OverrideValue::All),
            n => match n.parse::<u32>() {
                Ok(n) if n >= 1 => Ok(OverrideValue::Size(n)),
                _ => Err(format!(
                    "--batch value '{value}' is not none, all or a whole number of at least 1"
                )),
            },
        }
    }

    fn as_str(self) -> String {
        match self {
            OverrideValue::One => "none".to_string(),
            OverrideValue::Size(n) => n.to_string(),
            OverrideValue::All => "all".to_string(),
        }
    }
}

/// `--batch V` and `--batch node=V` (repeatable) of one run: what replaces the declared
/// `batch=` of the nodes that declare one. A node without `batch=` takes a single key and is
/// never batched by an override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchOverride {
    /// A bare `--batch V`: every node that declares `batch=`.
    pub all: Option<OverrideValue>,
    /// `--batch node=V`, by the name as written (a function name, `name=`, or a full node id).
    pub nodes: Vec<(String, OverrideValue)>,
}

impl BatchOverride {
    /// Parse the values of `--batch` (each `V` or `node=V`). A usage error names the value.
    pub fn parse(items: &[String]) -> Result<Self, String> {
        let mut out = BatchOverride::default();
        for item in items {
            match item.rsplit_once('=') {
                Some((node, value)) if !node.trim().is_empty() => {
                    out.nodes
                        .push((node.trim().to_string(), OverrideValue::parse(value)?));
                }
                _ => {
                    if out.all.is_some() {
                        return Err(format!(
                            "--batch is given twice without a node name ('{item}'); name the \
                             node of the second, as --batch node={item}"
                        ));
                    }
                    out.all = Some(OverrideValue::parse(item)?);
                }
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.nodes.is_empty()
    }

    /// Back to the flag's values (for a run sent to a server).
    pub fn to_flags(&self) -> Vec<String> {
        let mut out: Vec<String> = self.all.iter().map(|v| v.as_str()).collect();
        out.extend(
            self.nodes
                .iter()
                .map(|(n, v)| format!("{n}={}", v.as_str())),
        );
        out
    }

    /// Every named node must be in the graph and declare `batch=`. A usage error otherwise.
    pub fn check(&self, dag: &crate::dag::Dag) -> Result<(), String> {
        for (name, _) in &self.nodes {
            let found: Vec<&crate::model::DagNode> = dag
                .graph
                .node_weights()
                .filter(|n| names_node(n, name))
                .collect();
            let Some(node) = found.first() else {
                return Err(format!(
                    "--batch {name}=...: no node named '{name}' in the loaded pipeline"
                ));
            };
            if node.extracted.batch.is_none() {
                return Err(format!(
                    "--batch {name}=...: '{name}' does not declare batch=, so its function takes \
                     a single key; add batch= to it to batch it (barca docs partitions, \
                     \"Processing many keys per call\")"
                ));
            }
        }
        Ok(())
    }

    fn value_for(&self, node: &crate::model::DagNode) -> Option<OverrideValue> {
        self.nodes
            .iter()
            .rev()
            .find(|(name, _)| names_node(node, name))
            .map(|(_, v)| *v)
            .or(self.all)
    }
}

fn names_node(node: &crate::model::DagNode, name: &str) -> bool {
    node.id == name
        || node.extracted.function_name == name
        || node.extracted.explicit_name.as_deref() == Some(name)
        || crate::targets::short_name(&node.id) == name
}

// ─── Grouping ────────────────────────────────────────────────────────────────

/// How one node's keys are grouped in a run: its `batch=`, or the run's override of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grouping {
    One,
    Size(usize),
    All,
    Groups {
        groups: Vec<Vec<String>>,
        strict: bool,
    },
    By {
        source_text: String,
        max_size: Option<usize>,
    },
}

impl Grouping {
    /// As the dry run says it: `batch=all`, `batch=20`, `batch=none`.
    pub fn describe(&self) -> String {
        match self {
            Grouping::One => "batch=none".to_string(),
            Grouping::Size(n) => format!("batch={n}"),
            Grouping::All => "batch=all".to_string(),
            Grouping::Groups { .. } => "batch=batches(...)".to_string(),
            Grouping::By { source_text, .. } => format!("batch=batches_by({source_text})"),
        }
    }
}

/// The grouping of `node` in a run with `over`: `None` for a node that does not declare
/// `batch=` (one key per call, and its function takes a single key).
pub fn effective(node: &crate::model::DagNode, over: &BatchOverride) -> Option<Grouping> {
    let spec = node.extracted.batch.as_ref()?;
    if let Some(v) = over.value_for(node) {
        return Some(match v {
            OverrideValue::One => Grouping::One,
            OverrideValue::Size(n) => Grouping::Size(n as usize),
            OverrideValue::All => Grouping::All,
        });
    }
    Some(match spec {
        BatchSpec::Size { size } => Grouping::Size(*size as usize),
        BatchSpec::All => Grouping::All,
        BatchSpec::Groups { groups, strict } => Grouping::Groups {
            groups: groups.clone(),
            strict: *strict,
        },
        // Evaluated when the pipeline was loaded; a node left with the expression failed then
        // and is not planned.
        BatchSpec::GroupsExpr { strict, .. } => Grouping::Groups {
            groups: Vec::new(),
            strict: *strict,
        },
        BatchSpec::By {
            source_text,
            max_size,
        } => Grouping::By {
            source_text: source_text.clone(),
            max_size: max_size.map(|n| n as usize),
        },
    })
}

/// Group `keys` (each a key's inner display form, `retailer=r1`, in key order) into calls,
/// as indexes into `keys`. `labels` is required for [`Grouping::By`] (one per key).
///
/// Linear in the keys given: the keys that run, not the keys declared.
pub fn group_keys(
    keys: &[String],
    grouping: &Grouping,
    labels: Option<&[String]>,
) -> Result<Vec<Vec<usize>>, String> {
    let n = keys.len();
    Ok(match grouping {
        Grouping::One => (0..n).map(|i| vec![i]).collect(),
        Grouping::Size(size) => (0..n)
            .collect::<Vec<_>>()
            .chunks((*size).max(1))
            .map(<[usize]>::to_vec)
            .collect(),
        Grouping::All => {
            if n == 0 {
                Vec::new()
            } else {
                vec![(0..n).collect()]
            }
        }
        Grouping::Groups { groups, strict } => {
            let mut group_of: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for (g, members) in groups.iter().enumerate() {
                for m in members {
                    group_of.insert(m.as_str(), g);
                }
            }
            let mut calls: Vec<Vec<usize>> = Vec::new();
            let mut call_of_group: std::collections::HashMap<usize, usize> =
                std::collections::HashMap::new();
            let mut alone = Vec::new();
            for (i, key) in keys.iter().enumerate() {
                match group_of.get(key.as_str()) {
                    Some(g) => {
                        let c = *call_of_group.entry(*g).or_insert_with(|| {
                            calls.push(Vec::new());
                            calls.len() - 1
                        });
                        calls[c].push(i);
                    }
                    None if *strict => {
                        alone.push(key.clone());
                    }
                    None => calls.push(vec![i]),
                }
            }
            if !alone.is_empty() {
                let shown: Vec<&str> = alone.iter().take(5).map(String::as_str).collect();
                return Err(format!(
                    "batches(..., strict=True) names no group for the key{} {}{}",
                    if alone.len() == 1 { "" } else { "s" },
                    shown.join(", "),
                    if alone.len() > 5 {
                        format!(" and {} more", alone.len() - 5)
                    } else {
                        String::new()
                    }
                ));
            }
            calls
        }
        Grouping::By { max_size, .. } => {
            let labels = labels.ok_or("batches_by(...) labels were not evaluated")?;
            let mut calls: Vec<Vec<usize>> = Vec::new();
            let mut call_of: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for (i, label) in labels.iter().enumerate().take(n) {
                let c = *call_of.entry(label.as_str()).or_insert_with(|| {
                    calls.push(Vec::new());
                    calls.len() - 1
                });
                calls[c].push(i);
            }
            match max_size {
                Some(size) => calls
                    .into_iter()
                    .flat_map(|c| {
                        c.chunks((*size).max(1))
                            .map(<[usize]>::to_vec)
                            .collect::<Vec<_>>()
                    })
                    .collect(),
                None => calls,
            }
        }
    })
}

// ─── Python evaluation ───────────────────────────────────────────────────────

/// Evaluates `batches(<expression>)` to a list of groups, `batches_by(<function>)` to a label
/// per key, or checks that a `batches_by` function is callable. Reads a JSON request on stdin.
const SCRIPT: &str = r#"import json, sys
from barca._source_import import load_pipeline_module
_ns = dict(vars(load_pipeline_module(sys.argv[1])))
_ns['__builtins__'] = __builtins__
req = json.load(sys.stdin)
value = eval(req['expr'], _ns)
mode = req['mode']
def key_text(k):
    if isinstance(k, bool) or not isinstance(k, (str, int)):
        raise TypeError(f"batches(...) names the key {k!r} ({type(k).__name__}); a key is a string or an integer")
    return str(k)
if mode == 'groups':
    dims = req['dims']
    out = []
    for g in value:
        group = []
        for k in g:
            if isinstance(k, dict):
                group.append(','.join(f"{d}={key_text(k[d])}" for d in sorted(k)))
            elif len(dims) == 1:
                group.append(f"{dims[0]}={key_text(k)}")
            else:
                raise TypeError(f"batches(...) names the key {k!r} of a node with several dimensions; name it as a dict of dimension to value")
        out.append(group)
    print(json.dumps(out))
elif mode == 'check':
    if not callable(value):
        raise TypeError(f"batches_by({req['expr']}) is given {type(value).__name__}, not a function")
    print('[]')
else:
    labels = []
    for k in req['keys']:
        arg = k[req['dims'][0]] if len(req['dims']) == 1 else k
        label = value(arg)
        if isinstance(label, bool) or not isinstance(label, (str, int)):
            raise TypeError(f"batches_by({req['expr']}) returned {label!r} ({type(label).__name__}) for the key {arg!r}; a label must be a string or an integer")
        labels.append(str(label))
    print(json.dumps(labels))
"#;

fn run_python(
    python: &std::path::Path,
    source_file: &str,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    use std::io::Write;
    let module_path = std::path::Path::new(source_file)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(source_file));
    let mut cmd = std::process::Command::new(python);
    cmd.arg("-c")
        .arg(SCRIPT)
        .arg(module_path.to_string_lossy().as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = crate::helper_proc::spawn_std(&mut cmd)
        .map_err(|e| format!("cannot run {}: {e}", python.display()))?;
    let body = serde_json::to_vec(request).unwrap_or_default();
    if let Some(mut stdin) = child.stdin.take() {
        // A write error is reported by the exit status below.
        let _ = stdin.write_all(&body);
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("cannot run {}: {e}", python.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr
            .trim_end()
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty() && !l.starts_with(' '))
            .unwrap_or("an error")
            .trim()
            .to_string();
        return Err(last);
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("did not give JSON on stdout: {e}"))
}

/// Evaluate `batches(<expression>)` of `node` when its pipeline is loaded.
pub(crate) fn evaluate_groups(
    python: &std::path::Path,
    source_file: &str,
    expr: &str,
    dims: &[String],
) -> Result<Vec<Vec<String>>, String> {
    let value = run_python(
        python,
        source_file,
        &serde_json::json!({"mode": "groups", "expr": expr, "dims": dims}),
    )
    .map_err(|e| format!("batches({expr}) raised {e}"))?;
    let groups: Vec<Vec<String>> =
        serde_json::from_value(value).map_err(|e| format!("batches({expr}): {e}"))?;
    let mut seen = std::collections::HashSet::new();
    for g in &groups {
        if g.is_empty() {
            return Err(format!("batches({expr}) has an empty group"));
        }
        for k in g {
            if !seen.insert(k.clone()) {
                return Err(format!("batches({expr}) names the key {k} in two groups"));
            }
        }
    }
    Ok(groups)
}

/// Check that the function of `batches_by(<expression>)` can be found and called.
pub(crate) fn check_label_function(
    python: &std::path::Path,
    source_file: &str,
    expr: &str,
) -> Result<(), String> {
    run_python(
        python,
        source_file,
        &serde_json::json!({"mode": "check", "expr": expr}),
    )
    .map(|_| ())
    .map_err(|e| format!("batches_by({expr}) raised {e}"))
}

/// The `batches_by` label of each key (inner display forms, `retailer=r1`).
pub fn evaluate_labels(
    python: &std::path::Path,
    source_file: &str,
    expr: &str,
    keys: &[String],
) -> Result<Vec<String>, String> {
    let parsed: Vec<serde_json::Map<String, serde_json::Value>> = keys
        .iter()
        .map(|k| {
            let id = format!("node[{k}]");
            crate::model::PartitionKey::parse_from_id(&id)
                .1
                .0
                .into_iter()
                .map(|(d, v)| (d, serde_json::Value::String(v)))
                .collect()
        })
        .collect();
    let dims: Vec<String> = parsed
        .first()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let value = run_python(
        python,
        source_file,
        &serde_json::json!({"mode": "labels", "expr": expr, "dims": dims, "keys": parsed}),
    )
    .map_err(|e| format!("batches_by({expr}) raised {e}"))?;
    let labels: Vec<String> =
        serde_json::from_value(value).map_err(|e| format!("batches_by({expr}): {e}"))?;
    if labels.len() != keys.len() {
        return Err(format!("batches_by({expr}) did not label every key"));
    }
    Ok(labels)
}

// ─── Dry run ─────────────────────────────────────────────────────────────────

/// How many calls `n` keys make under `grouping`, when that follows from the count alone.
fn calls_for(n: usize, grouping: &Grouping) -> Option<usize> {
    match grouping {
        Grouping::One => Some(n),
        Grouping::Size(size) => Some(n.div_ceil((*size).max(1))),
        Grouping::All => Some(usize::from(n > 0)),
        _ => None,
    }
}

/// "3 keys in 1 call (batch=all)".
pub fn grouping_line(plan: &crate::results::BatchPlan) -> String {
    let keys = format!("{} key{}", plan.keys, if plan.keys == 1 { "" } else { "s" });
    match plan.calls {
        Some(c) => format!(
            "{keys} in {c} call{} ({})",
            if c == 1 { "" } else { "s" },
            plan.grouping
        ),
        None => format!("{keys} in calls by {}", plan.grouping),
    }
}

/// Add the grouping of each batched node's keys that will run to its dry-run report: `batch`,
/// and the line at the end of `detail`.
pub fn annotate(
    dag: &crate::dag::Dag,
    steps: &mut [crate::results::StepReport],
    over: &BatchOverride,
    python: &std::path::Path,
) -> Result<(), crate::BarcaError> {
    for step in steps.iter_mut() {
        let Some(node) = dag.get_node(&step.id) else {
            continue;
        };
        let Some(grouping) = effective(node, over) else {
            continue;
        };
        let Some(parts) = step.partitions.as_ref() else {
            continue;
        };
        let n = parts.will_run;
        if n == 0 {
            continue;
        }
        let mut calls = calls_for(n, &grouping);
        if calls.is_none() && parts.will_run_keys.len() == n {
            let labels = match &grouping {
                Grouping::By { source_text, .. } => evaluate_labels(
                    python,
                    &node.extracted.source_file,
                    source_text,
                    &parts.will_run_keys,
                )
                .map(Some)
                .map_err(|e| crate::BarcaError::Usage(format!("{}: {e}", step.id)))?,
                _ => None,
            };
            calls = Some(
                group_keys(&parts.will_run_keys, &grouping, labels.as_deref())
                    .map_err(|e| crate::BarcaError::Usage(format!("{}: {e}", step.id)))?
                    .len(),
            );
        }
        let plan = crate::results::BatchPlan {
            grouping: grouping.describe(),
            keys: n,
            calls,
        };
        let line = grouping_line(&plan);
        step.detail = Some(match step.detail.take() {
            Some(d) if !d.is_empty() => format!("{d}; {line}"),
            _ => line,
        });
        step.batch = Some(plan);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("r=k{i}")).collect()
    }

    #[test]
    fn sizes_chunk_in_key_order() {
        assert_eq!(
            group_keys(&keys(5), &Grouping::Size(2), None).unwrap(),
            vec![vec![0, 1], vec![2, 3], vec![4]]
        );
        assert_eq!(
            group_keys(&keys(3), &Grouping::All, None).unwrap(),
            vec![vec![0, 1, 2]]
        );
        assert!(group_keys(&[], &Grouping::All, None).unwrap().is_empty());
        assert_eq!(
            group_keys(&keys(2), &Grouping::One, None).unwrap(),
            vec![vec![0], vec![1]]
        );
    }

    #[test]
    fn explicit_groups_keep_together_and_strays_run_alone_or_fail() {
        let g = Grouping::Groups {
            groups: vec![vec!["r=k2".into(), "r=k0".into()], vec!["r=k9".into()]],
            strict: false,
        };
        assert_eq!(
            group_keys(&keys(3), &g, None).unwrap(),
            vec![vec![0, 2], vec![1]]
        );
        let strict = Grouping::Groups {
            groups: vec![vec!["r=k0".into()]],
            strict: true,
        };
        let err = group_keys(&keys(2), &strict, None).unwrap_err();
        assert!(err.contains("r=k1"), "{err}");
    }

    #[test]
    fn labels_group_then_split_by_max_size() {
        let labels: Vec<String> = ["a", "b", "a", "a", "b"].map(String::from).to_vec();
        let g = Grouping::By {
            source_text: "f".into(),
            max_size: Some(2),
        };
        assert_eq!(
            group_keys(&keys(5), &g, Some(&labels)).unwrap(),
            vec![vec![0, 2], vec![3], vec![1, 4]]
        );
    }

    #[test]
    fn overrides_parse_and_name_nodes() {
        let o = BatchOverride::parse(&["all".into(), "scored=3".into(), "x=none".into()]).unwrap();
        assert_eq!(o.all, Some(OverrideValue::All));
        assert_eq!(
            o.nodes,
            vec![
                ("scored".to_string(), OverrideValue::Size(3)),
                ("x".to_string(), OverrideValue::One)
            ]
        );
        assert_eq!(o.to_flags(), vec!["all", "scored=3", "x=none"]);
        assert!(BatchOverride::parse(&["0".into()]).is_err());
        assert!(BatchOverride::parse(&["many".into()]).is_err());
        assert!(BatchOverride::parse(&["all".into(), "3".into()]).is_err());
    }
}
