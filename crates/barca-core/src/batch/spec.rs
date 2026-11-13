//! The compile side of batched partitions (RFC-0013): reading `batch=` from the source
//! ([`extract`]), the per-run override (`--batch`, [`BatchOverride`]), and the Python
//! evaluation of `batches(<expression>)` and `batches_by(<function>)` that loading a pipeline
//! runs. Grouping the keys of a run into calls is the parent module's.

use ruff_python_ast::{self as ast, Expr};
use ruff_text_size::Ranged;

use crate::model::BatchSpec;

// ─── Reading `batch=` ────────────────────────────────────────────────────────

/// The fix shown with every invalid `batch=`.
pub(crate) const FIX: &str = "write batch=N (a whole number of at least 1), batch=\"all\", \
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

    pub(super) fn value_for(&self, node: &crate::model::DagNode) -> Option<OverrideValue> {
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
        labels.append(json.dumps(label))
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
    let mut child = crate::spawn_lock::spawn_std(&mut cmd)
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
pub(crate) fn evaluate_labels(
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
