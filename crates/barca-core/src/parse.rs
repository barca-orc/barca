//! Python source parsing — extracts decorated nodes using ruff's AST.
//!
//! Pure function: `(source, file_path) → Result<Vec<ExtractedNode>, ParseError>`.
//! No I/O, no side effects.

use ruff_python_ast::{self as ast, Expr, Keyword, Stmt};
use ruff_python_parser::parse_module;
use ruff_text_size::Ranged;
use smallvec::SmallVec;
use std::collections::HashMap;

use crate::model::{
    CronExpr, DeclaredInput, ExtractedNode, Freshness, NodeKind, NodeRef, ParallelCall,
    PartitionSpec, PartitionValue, SerializerKind, SinkDecl, ValueType,
};

/// Error from parsing a Python source file.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("syntax error in {file}: {message}")]
    SyntaxError { file: String, message: String },

    #[error("{file}: {function}: invalid Schedule cron {cron:?} — {reason}")]
    InvalidCron {
        file: String,
        function: String,
        cron: String,
        reason: String,
    },

    #[error(
        "{file}: {function}: invalid env= — {reason}. Declare environment variables as a literal \
         list of strings, e.g. env=[\"SOURCE_CSV\", \"API_TOKEN\"]"
    )]
    InvalidEnv {
        file: String,
        function: String,
        reason: String,
    },

    /// A barca decorator or helper is called with an argument it does not define (#284).
    /// The first line says what is wrong, the second what to do.
    #[error("{file}:{function} (line {line}): {message}\n{fix}")]
    InvalidArguments {
        file: String,
        function: String,
        line: usize,
        message: String,
        fix: String,
    },
}

/// Parse a Python source file and extract all barca-decorated nodes.
///
/// Pure function — no I/O, no side effects. Returns `Err` for unparseable Python.
/// Returns `Ok(vec![])` for valid Python with no barca decorators.
pub fn extract_nodes(source: &str, file_path: &str) -> Result<Vec<ExtractedNode>, ParseError> {
    let parsed = parse_module(source).map_err(|e| ParseError::SyntaxError {
        file: file_path.to_string(),
        message: e.to_string(),
    })?;

    let module = parsed.into_syntax();
    let mut nodes = Vec::new();
    let names = FileNames::collect(&module.body);

    for stmt in &module.body {
        if let Stmt::FunctionDef(func) = stmt
            && let Some(extracted) = try_extract_function(func, file_path, source, &names)?
        {
            // Cone hash computed later in build_dag with cached module definitions.
            nodes.push(extracted);
        }
    }

    Ok(nodes)
}

/// Parse organizational metadata and nodes together, without importing the module.
pub(crate) fn extract_pipeline(
    source: &str,
    file: &str,
) -> Result<(Vec<ExtractedNode>, Vec<crate::groups::Declaration>), crate::BarcaError> {
    let parsed =
        parse_module(source).map_err(|e| crate::BarcaError::Parse(format!("{file}: {e}")))?;
    let body = &parsed.syntax().body;
    let names = FileNames::collect(body);
    let mut nodes = Vec::new();
    for stmt in body {
        if let Stmt::FunctionDef(func) = stmt
            && let Some(node) = try_extract_function(func, file, source, &names)
                .map_err(|e| crate::BarcaError::Parse(e.to_string()))?
        {
            nodes.push(node);
        }
    }
    Ok((nodes, crate::groups::extract_body(body, file)?))
}

/// What the top-level names of a file are bound to, for resolving `inputs=` references:
/// functions defined in the file, names imported with `from M import x [as y]`, and modules
/// imported with `import M [as m]`. Module names keep their leading dots (`.sources`).
#[derive(Default)]
pub(crate) struct FileNames {
    local: std::collections::HashSet<String>,
    /// local name -> (module, name in that module)
    from_imports: HashMap<String, (String, String)>,
    /// local dotted name -> module (`s` -> `pipelines.sources`, `a.b` -> `a.b`)
    modules: HashMap<String, String>,
    /// The decorator and helper names that are positively barca's, for the argument check.
    pub(crate) barca: crate::decorator_args::BarcaNames,
}

impl FileNames {
    pub(crate) fn collect(body: &[Stmt]) -> Self {
        let mut names = FileNames {
            barca: crate::decorator_args::BarcaNames::of(body),
            ..FileNames::default()
        };
        for stmt in body {
            match stmt {
                Stmt::FunctionDef(f) => {
                    names.local.insert(f.name.to_string());
                }
                Stmt::ImportFrom(imp) => {
                    let module = format!(
                        "{}{}",
                        ".".repeat(imp.level as usize),
                        imp.module.as_ref().map(|m| m.as_str()).unwrap_or("")
                    );
                    for alias in &imp.names {
                        let name = alias.name.to_string();
                        let local = alias
                            .asname
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| name.clone());
                        names.from_imports.insert(local, (module.clone(), name));
                    }
                }
                Stmt::Import(imp) => {
                    for alias in &imp.names {
                        let module = alias.name.to_string();
                        let local = alias
                            .asname
                            .as_ref()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|| module.clone());
                        names.modules.insert(local, module);
                    }
                }
                _ => {}
            }
        }
        names
    }

    /// The node an `inputs=` value (or `collect(...)` / `partitions_from(...)` argument) refers
    /// to: a function in this file, a name imported from a module, or `module.name`.
    pub(crate) fn node_ref(&self, expr: &Expr) -> Option<NodeRef> {
        match expr {
            Expr::Name(n) => {
                let id = n.id.to_string();
                if self.local.contains(&id) {
                    return Some(NodeRef::FunctionName(id));
                }
                Some(match self.from_imports.get(&id) {
                    Some((module, name)) => NodeRef::Imported {
                        module: module.clone(),
                        name: name.clone(),
                    },
                    None => NodeRef::FunctionName(id),
                })
            }
            Expr::Attribute(attr) => {
                let dotted = dotted_expr(&attr.value)?;
                let module = self.modules.get(&dotted)?;
                Some(NodeRef::Imported {
                    module: module.clone(),
                    name: attr.attr.to_string(),
                })
            }
            _ => None,
        }
    }
}

/// `a.b.c` as a string, for a chain of names.
fn dotted_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(format!("{}.{}", dotted_expr(&a.value)?, a.attr)),
        _ => None,
    }
}

fn try_extract_function(
    func: &ast::StmtFunctionDef,
    file_path: &str,
    source: &str,
    names: &FileNames,
) -> Result<Option<ExtractedNode>, ParseError> {
    let mut kind = None;
    let mut keywords: Vec<&Keyword> = Vec::new();
    let mut sinks: SmallVec<[SinkDecl; 2]> = SmallVec::new();
    let mut is_unsafe = false;

    for decorator in &func.decorator_list {
        // Check for @unsafe
        if is_unsafe_decorator(&decorator.expression, &names.barca) {
            is_unsafe = true;
            continue;
        }

        // Check for @sink(...)
        if let Some(sink) = try_extract_sink(&decorator.expression, &names.barca) {
            sinks.push(sink);
            continue;
        }

        // Check for @asset/@sensor/@task
        if let Some((k, kws)) = match_node_decorator(&decorator.expression, &names.barca) {
            kind = Some(k);
            keywords = kws;
        }
    }

    let Some(kind) = kind else {
        return Ok(None);
    };

    // Before anything is read from the arguments: an argument barca does not define is an
    // error, not something to ignore (`decorator_args`).
    if let Some(problem) =
        crate::decorator_args::check_decorators(&func.decorator_list, &names.barca)
    {
        return Err(ParseError::InvalidArguments {
            file: file_path.to_string(),
            function: func.name.to_string(),
            line: source[..problem.offset].matches('\n').count() + 1,
            message: problem.message,
            fix: problem.fix,
        });
    }

    let freshness = extract_freshness(&keywords, file_path, func.name.as_str(), &names.barca)?
        .unwrap_or(Freshness::default_for(kind));
    let inputs = extract_inputs(&keywords, names);
    let partitions = extract_partitions(&keywords, source, names);
    let explicit_name = extract_string_kwarg(&keywords, "name");
    let description = extract_string_kwarg(&keywords, "description");
    let timeout_seconds = extract_int_kwarg(&keywords, "timeout_seconds").unwrap_or(300);
    // `retries` is the total number of attempts (1 = no retry). Clamp 0 → 1.
    let retries = extract_int_kwarg(&keywords, "retries").unwrap_or(1).max(1);
    let retry_backoff_seconds = extract_float_kwarg(&keywords, "retry_backoff").unwrap_or(0.0);
    let tags = extract_tags(&keywords);
    let env = extract_env(&keywords, file_path, func.name.as_str())?;
    let artifact_serializer = keywords
        .iter()
        .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("serializer"))
        .and_then(|kw| extract_serializer_kind(&kw.value));

    let parallel_calls = if kind == NodeKind::Task {
        extract_parallel_calls(&func.body, &names.barca.in_function(func))
    } else {
        Vec::new()
    };
    let (param_types, return_type) = extract_type_annotations(func);
    let unused_inputs =
        crate::unused_inputs::unused_inputs(func, &inputs, &param_types, &|local| {
            use crate::unused_inputs::Imported;
            if let Some((module, name)) = names.from_imports.get(local) {
                return Some(Imported::Name { module, name });
            }
            names.modules.get(local).map(|m| Imported::Module(m))
        });

    let start = func.range().start().to_usize();
    // What the definition hash covers: the function from `def` on and the decorator parts that
    // count, in canonical form. Never the decorators as written (`crate::definition`).
    let source_text = crate::definition::node_definition(func, source, &names.barca)
        .map(|definition| definition.text)
        .unwrap_or_default();

    Ok(Some(ExtractedNode {
        kind,
        function_name: func.name.to_string(),
        explicit_name,
        freshness,
        inputs,
        partitions,
        sinks,
        timeout_seconds,
        retries,
        retry_backoff_seconds,
        description,
        tags,
        is_unsafe,
        source_file: file_path.to_string(),
        byte_offset: start,
        source_text,
        cone_hash: String::new(), // computed after extraction in extract_nodes()
        artifact_serializer,
        param_types,
        return_type,
        parallel_calls,
        env,
        unused_inputs,
    }))
}

/// Extract parameter and return types from function annotations.
fn extract_type_annotations(
    func: &ast::StmtFunctionDef,
) -> (HashMap<String, ValueType>, Option<ValueType>) {
    let mut param_types = HashMap::new();
    let params = &func.parameters;

    for arg in params
        .posonlyargs
        .iter()
        .chain(&params.args)
        .chain(&params.kwonlyargs)
    {
        if let Some(vt) = arg.annotation().and_then(parse_value_type_expr) {
            param_types.insert(arg.name().id.to_string(), vt);
        }
    }

    let return_type = func.returns.as_deref().and_then(parse_value_type_expr);

    (param_types, return_type)
}

/// Map a type annotation expression to a supported [`ValueType`].
fn parse_value_type_expr(expr: &Expr) -> Option<ValueType> {
    match expr {
        Expr::StringLiteral(s) => parse_type_path(&s.value.to_string()),
        Expr::Subscript(sub) => {
            // list[pl.DataFrame] from collect() fan-in params.
            if is_list_annotation(sub.value.as_ref()) {
                parse_value_type_expr(&sub.slice)
            } else {
                None
            }
        }
        Expr::Attribute(attr) => {
            let module = expr_to_name(attr.value.as_ref())?;
            parse_type_path(&format!("{module}.{}", attr.attr))
        }
        Expr::Name(name) => parse_type_path(&name.id),
        _ => None,
    }
}

fn is_list_annotation(expr: &Expr) -> bool {
    matches!(expr, Expr::Name(n) if n.id.as_str() == "list")
}

fn expr_to_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        _ => None,
    }
}

fn parse_type_path(path: &str) -> Option<ValueType> {
    let path = path.trim();
    let (module, name) = match path.rsplit_once('.') {
        Some((m, n)) => (m, n),
        None => ("", path),
    };
    classify_type(module, name)
}

fn classify_type(module: &str, name: &str) -> Option<ValueType> {
    match (module, name) {
        ("pl" | "polars", "DataFrame") => Some(ValueType::Polars),
        ("pl" | "polars", "LazyFrame") => Some(ValueType::PolarsLazy),
        ("pd" | "pandas", "DataFrame") => Some(ValueType::Pandas),
        ("pyarrow", "Table") => Some(ValueType::PyArrow),
        ("duckdb", "DuckDBPyRelation") => Some(ValueType::DuckDB),
        _ => None,
    }
}

pub(crate) fn is_unsafe_decorator(expr: &Expr, barca: &crate::decorator_args::BarcaNames) -> bool {
    barca.recognized(expr) == Some("unsafe")
}

pub(crate) fn try_extract_sink(
    expr: &Expr,
    barca: &crate::decorator_args::BarcaNames,
) -> Option<SinkDecl> {
    if let Expr::Call(call) = expr
        && barca.recognized(&call.func) == Some("sink")
    {
        let path = call
            .arguments
            .args
            .first()
            .and_then(extract_string_literal)?;
        let serializer = call
            .arguments
            .keywords
            .iter()
            .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("serializer"))
            .and_then(|kw| extract_serializer_kind(&kw.value));
        return Some(SinkDecl { path, serializer });
    }
    None
}

fn extract_serializer_kind(expr: &Expr) -> Option<SerializerKind> {
    let name = match expr {
        Expr::StringLiteral(s) => s.value.to_string(),
        Expr::Name(n) => n.id.to_string(),
        _ => return None,
    };
    match name.to_lowercase().as_str() {
        "json" => Some(SerializerKind::Json),
        "parquet" => Some(SerializerKind::Parquet),
        "pickle" => Some(SerializerKind::Pickle),
        "text" => Some(SerializerKind::Text),
        "yaml" => Some(SerializerKind::Yaml),
        _ => None,
    }
}

pub(crate) fn match_node_decorator<'a>(
    expr: &'a Expr,
    barca: &crate::decorator_args::BarcaNames,
) -> Option<(NodeKind, Vec<&'a Keyword>)> {
    let (callee, keywords) = match expr {
        Expr::Call(call) => (call.func.as_ref(), call.arguments.keywords.iter().collect()),
        _ => (expr, Vec::new()),
    };
    let kind = match barca.recognized(callee)? {
        "asset" => NodeKind::Asset,
        "sensor" => NodeKind::Sensor,
        "task" => NodeKind::Task,
        _ => return None,
    };
    Some((kind, keywords))
}

fn extract_freshness(
    keywords: &[&Keyword],
    file_path: &str,
    function_name: &str,
    barca: &crate::decorator_args::BarcaNames,
) -> Result<Option<Freshness>, ParseError> {
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() != "freshness" {
            continue;
        }
        let (callee, call) = match &kw.value {
            Expr::Call(call) => (call.func.as_ref(), Some(call)),
            expr => (expr, None),
        };
        let freshness = match barca.recognized(callee) {
            Some("Always") => Freshness::Always,
            Some("Manual") => Freshness::Manual,
            Some("Schedule") => {
                let Some(call) = call else { return Ok(None) };
                let cron = call
                    .arguments
                    .args
                    .first()
                    .and_then(extract_string_literal)
                    .unwrap_or_default();
                if let Err(reason) = CronExpr::validate(&cron) {
                    return Err(ParseError::InvalidCron {
                        file: file_path.to_string(),
                        function: function_name.to_string(),
                        cron,
                        reason,
                    });
                }
                Freshness::Schedule(CronExpr(cron))
            }
            _ => return Ok(None),
        };
        return Ok(Some(freshness));
    }
    Ok(None)
}

fn extract_inputs(keywords: &[&Keyword], names: &FileNames) -> SmallVec<[DeclaredInput; 4]> {
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() != "inputs" {
            continue;
        }
        if let Expr::Dict(dict) = &kw.value {
            return extract_inputs_from_dict(dict, names);
        }
    }
    SmallVec::new()
}

fn extract_inputs_from_dict(
    dict: &ast::ExprDict,
    names: &FileNames,
) -> SmallVec<[DeclaredInput; 4]> {
    let mut inputs = SmallVec::new();

    for item in &dict.items {
        let Some(ref key_expr) = item.key else {
            continue;
        };
        let param_name = match extract_string_literal(key_expr) {
            Some(s) => s,
            None => continue,
        };

        let (upstream, collected) = match &item.value {
            e @ (Expr::Name(_) | Expr::Attribute(_)) => match names.node_ref(e) {
                Some(r) => (r, false),
                None => continue,
            },
            Expr::Call(call) => {
                let is_collect = names.barca.recognized(&call.func) == Some("collect");
                if is_collect {
                    match call.arguments.args.first().and_then(|a| names.node_ref(a)) {
                        Some(r) => (r, true),
                        None => continue,
                    }
                } else if names.barca.recognized(&call.func) == Some("asset_ref") {
                    if let Some(arg) = call.arguments.args.first() {
                        if let Some(s) = extract_string_literal(arg) {
                            (NodeRef::Canonical(s), false)
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                } else if let Expr::Name(n) = call.func.as_ref() {
                    (NodeRef::FunctionName(n.id.to_string()), false)
                } else {
                    continue;
                }
            }
            _ => continue,
        };

        inputs.push(DeclaredInput {
            param_name,
            upstream,
            collected,
        });
    }

    inputs
}

fn extract_partitions(
    keywords: &[&Keyword],
    source: &str,
    names: &FileNames,
) -> HashMap<String, PartitionSpec> {
    let mut result = HashMap::new();

    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() != "partitions" {
            continue;
        }
        if let Expr::Dict(dict) = &kw.value {
            for item in &dict.items {
                let Some(ref key_expr) = item.key else {
                    continue;
                };
                let key = match extract_string_literal(key_expr) {
                    Some(s) => s,
                    None => continue,
                };

                let spec = match &item.value {
                    Expr::Call(call) => {
                        if let Some(name) = names.barca.recognized(&call.func) {
                            match name {
                                "partitions" => extract_partition_spec(call, source),
                                "partitions_from" => {
                                    let source_ref =
                                        call.arguments.args.first().and_then(|a| names.node_ref(a));
                                    if let Some(source_ref) = source_ref {
                                        PartitionSpec::DerivedFrom { source_ref }
                                    } else {
                                        continue;
                                    }
                                }
                                _ => continue,
                            }
                        } else {
                            continue;
                        }
                    }
                    _ => continue,
                };

                result.insert(key, spec);
            }
        }
    }

    result
}

/// Extract a PartitionSpec from a `partitions(...)` call.
/// If the argument is a literal list, extract values statically.
/// Otherwise, extract the source text for dynamic Python evaluation at plan time.
fn extract_partition_spec(call: &ast::ExprCall, source: &str) -> PartitionSpec {
    let Some(first_arg) = call.arguments.args.first() else {
        return PartitionSpec::Static { values: vec![] };
    };

    // Try static extraction first (literal list).
    if let Expr::List(_) = first_arg {
        let values = extract_partition_values(first_arg);
        return PartitionSpec::Static { values };
    }

    // Non-literal (ListComp, Call, etc.) — extract source text for Python eval.
    let start = first_arg.range().start().to_usize();
    let end = first_arg.range().end().to_usize();
    let source_text = source[start..end].to_string();
    PartitionSpec::Dynamic { source_text }
}

fn extract_partition_values(expr: &Expr) -> Vec<PartitionValue> {
    if let Expr::List(list) = expr {
        list.elts
            .iter()
            .filter_map(|e| match e {
                Expr::StringLiteral(s) => Some(PartitionValue::Str(s.value.to_string())),
                Expr::NumberLiteral(n) => {
                    if let ast::Number::Int(i) = &n.value {
                        i.as_i64().map(PartitionValue::Int)
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect()
    } else {
        vec![]
    }
}

fn extract_tags(keywords: &[&Keyword]) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() != "tags" {
            continue;
        }
        if let Expr::Dict(dict) = &kw.value {
            for item in &dict.items {
                let Some(ref key_expr) = item.key else {
                    continue;
                };
                if let (Some(k), Some(v)) = (
                    extract_string_literal(key_expr),
                    extract_string_literal(&item.value),
                ) {
                    tags.insert(k, v);
                }
            }
        }
    }
    tags
}

/// `env=["NAME", ...]`: a literal list of string literals, read statically. Anything else (a
/// variable, a call, a tuple, a non-string element) is a parse error, never silently ignored —
/// an undeclared variable would silently drop out of the run hash.
fn extract_env(
    keywords: &[&Keyword],
    file_path: &str,
    function_name: &str,
) -> Result<Vec<String>, ParseError> {
    let err = |reason: String| ParseError::InvalidEnv {
        file: file_path.to_string(),
        function: function_name.to_string(),
        reason,
    };
    let Some(kw) = keywords
        .iter()
        .find(|kw| kw.arg.as_ref().map(|a| a.as_str()) == Some("env"))
    else {
        return Ok(Vec::new());
    };
    let Expr::List(list) = &kw.value else {
        return Err(err("expected a list literal".to_string()));
    };
    let mut names = Vec::with_capacity(list.elts.len());
    for elt in &list.elts {
        let Some(name) = extract_string_literal(elt) else {
            return Err(err("every element must be a string literal".to_string()));
        };
        if name.is_empty() || name.contains('=') || name.contains('\0') {
            return Err(err(format!(
                "{name:?} is not a valid environment variable name"
            )));
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

fn extract_string_kwarg(keywords: &[&Keyword], name: &str) -> Option<String> {
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() == name {
            return extract_string_literal(&kw.value);
        }
    }
    None
}

fn extract_int_kwarg(keywords: &[&Keyword], name: &str) -> Option<u32> {
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() == name
            && let Expr::NumberLiteral(n) = &kw.value
            && let ast::Number::Int(i) = &n.value
        {
            return i.as_u32();
        }
    }
    None
}

/// Extract a float kwarg, accepting both float (`2.0`) and int (`2`) literals.
fn extract_float_kwarg(keywords: &[&Keyword], name: &str) -> Option<f64> {
    for kw in keywords {
        let Some(ref ident) = kw.arg else { continue };
        if ident.as_str() == name
            && let Expr::NumberLiteral(n) = &kw.value
        {
            return match &n.value {
                ast::Number::Float(f) => Some(*f),
                ast::Number::Int(i) => i.as_u32().map(|v| v as f64),
                _ => None,
            };
        }
    }
    None
}

fn extract_string_literal(expr: &Expr) -> Option<String> {
    if let Expr::StringLiteral(s) = expr {
        Some(s.value.to_string())
    } else {
        None
    }
}

/// Scan a task function body for `parallel(...)` and `parallel_map(...)` calls.
/// Returns a list of ParallelCall structs describing each call.
fn extract_parallel_calls(
    body: &[Stmt],
    barca: &crate::decorator_args::BarcaNames,
) -> Vec<ParallelCall> {
    let mut results = Vec::new();
    collect_parallel_calls_from_stmts(body, &mut results, barca);
    results
}

/// Recursively walk statements looking for parallel()/parallel_map() calls.
fn collect_parallel_calls_from_stmts(
    stmts: &[Stmt],
    results: &mut Vec<ParallelCall>,
    barca: &crate::decorator_args::BarcaNames,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Expr(expr_stmt) => {
                collect_parallel_calls_from_expr(&expr_stmt.value, results, barca);
            }
            Stmt::Assign(assign) => {
                collect_parallel_calls_from_expr(&assign.value, results, barca);
            }
            Stmt::AnnAssign(assign) => {
                if let Some(ref value) = assign.value {
                    collect_parallel_calls_from_expr(value, results, barca);
                }
            }
            Stmt::Return(ret) => {
                if let Some(ref value) = ret.value {
                    collect_parallel_calls_from_expr(value, results, barca);
                }
            }
            Stmt::If(if_stmt) => {
                collect_parallel_calls_from_stmts(&if_stmt.body, results, barca);
                for clause in &if_stmt.elif_else_clauses {
                    collect_parallel_calls_from_stmts(&clause.body, results, barca);
                }
            }
            Stmt::For(for_stmt) => {
                collect_parallel_calls_from_stmts(&for_stmt.body, results, barca);
                collect_parallel_calls_from_stmts(&for_stmt.orelse, results, barca);
            }
            Stmt::While(while_stmt) => {
                collect_parallel_calls_from_stmts(&while_stmt.body, results, barca);
                collect_parallel_calls_from_stmts(&while_stmt.orelse, results, barca);
            }
            Stmt::With(with_stmt) => {
                collect_parallel_calls_from_stmts(&with_stmt.body, results, barca);
            }
            Stmt::Try(try_stmt) => {
                collect_parallel_calls_from_stmts(&try_stmt.body, results, barca);
                for handler in &try_stmt.handlers {
                    let ast::ExceptHandler::ExceptHandler(h) = handler;
                    collect_parallel_calls_from_stmts(&h.body, results, barca);
                }
                collect_parallel_calls_from_stmts(&try_stmt.orelse, results, barca);
                collect_parallel_calls_from_stmts(&try_stmt.finalbody, results, barca);
            }
            Stmt::Match(m) => {
                for case in &m.cases {
                    collect_parallel_calls_from_stmts(&case.body, results, barca);
                }
            }
            Stmt::AugAssign(a) => {
                collect_parallel_calls_from_expr(&a.value, results, barca);
            }
            _ => {}
        }
    }
}

/// Check if an expression is a call to `parallel()` or `parallel_map()` and extract it.
/// Recursively descends into sub-expressions (call args, ternaries, lists, tuples,
/// list comprehensions) to find nested parallel() calls.
fn collect_parallel_calls_from_expr(
    expr: &Expr,
    results: &mut Vec<ParallelCall>,
    barca: &crate::decorator_args::BarcaNames,
) {
    if let Expr::Call(call) = expr {
        if let Some(name) = barca.recognized(&call.func) {
            match name {
                "parallel" => {
                    results.push(extract_parallel_call(call));
                    return;
                }
                "parallel_map" => {
                    results.push(extract_parallel_map_call(call));
                    return;
                }
                _ => {}
            }
        }
        // Not a parallel/parallel_map call — descend into call arguments
        for arg in &call.arguments.args {
            collect_parallel_calls_from_expr(arg, results, barca);
        }
        return;
    }

    // Descend into other expression forms
    match expr {
        Expr::If(e) => {
            collect_parallel_calls_from_expr(&e.body, results, barca);
            collect_parallel_calls_from_expr(&e.test, results, barca);
            collect_parallel_calls_from_expr(&e.orelse, results, barca);
        }
        Expr::List(l) => {
            for elt in &l.elts {
                collect_parallel_calls_from_expr(elt, results, barca);
            }
        }
        Expr::Tuple(t) => {
            for elt in &t.elts {
                collect_parallel_calls_from_expr(elt, results, barca);
            }
        }
        Expr::ListComp(lc) => {
            collect_parallel_calls_from_expr(&lc.elt, results, barca);
        }
        _ => {}
    }
}

/// Extract a ParallelCall from a `parallel(...)` call expression.
fn extract_parallel_call(call: &ast::ExprCall) -> ParallelCall {
    let mut static_refs = Vec::new();
    let mut is_dynamic = false;

    for arg in &call.arguments.args {
        match arg {
            // partial(func_name, ...) or functools.partial(func_name, ...) — extract func_name
            Expr::Call(inner_call) => {
                let is_partial = match inner_call.func.as_ref() {
                    Expr::Name(n) => n.id.as_str() == "partial",
                    Expr::Attribute(a) => a.attr.as_str() == "partial",
                    _ => false,
                };
                if is_partial
                    && let Some(first_arg) = inner_call.arguments.args.first()
                    && let Expr::Name(func_name) = first_arg
                {
                    static_refs.push(NodeRef::FunctionName(func_name.id.to_string()));
                }
            }
            // *expr — starred argument, always dynamic
            Expr::Starred(starred) => {
                is_dynamic = true;
                // Try to extract static_refs from the starred expression.
                // e.g., *(partial(deploy, r) for r in regions) — extract "deploy"
                extract_refs_from_starred(&starred.value, &mut static_refs);
            }
            _ => {}
        }
    }

    ParallelCall {
        static_refs,
        is_dynamic,
    }
}

/// Extract a ParallelCall from a `parallel_map(func, items, ...)` call expression.
fn extract_parallel_map_call(call: &ast::ExprCall) -> ParallelCall {
    let mut static_refs = Vec::new();

    // First arg is the function reference
    if let Some(first_arg) = call.arguments.args.first()
        && let Expr::Name(func_name) = first_arg
    {
        static_refs.push(NodeRef::FunctionName(func_name.id.to_string()));
    }

    // parallel_map is always dynamic (items resolved at runtime)
    ParallelCall {
        static_refs,
        is_dynamic: true,
    }
}

/// Try to extract function references from a starred expression.
/// Handles patterns like:
///   *(partial(deploy, r) for r in regions)  → extracts "deploy"
///   *work_items                              → no refs extractable
fn extract_refs_from_starred(expr: &Expr, refs: &mut Vec<NodeRef>) {
    match expr {
        // Generator expression: (partial(func, ...) for ... in ...)
        Expr::Generator(genexpr) => {
            if let Expr::Call(inner_call) = genexpr.elt.as_ref() {
                let is_partial = match inner_call.func.as_ref() {
                    Expr::Name(n) => n.id.as_str() == "partial",
                    Expr::Attribute(a) => a.attr.as_str() == "partial",
                    _ => false,
                };
                if is_partial
                    && let Some(first_arg) = inner_call.arguments.args.first()
                    && let Expr::Name(func_name) = first_arg
                {
                    refs.push(NodeRef::FunctionName(func_name.id.to_string()));
                }
            }
        }
        // List comprehension: [partial(func, ...) for ... in ...]
        Expr::ListComp(comp) => {
            if let Expr::Call(inner_call) = comp.elt.as_ref() {
                let is_partial = match inner_call.func.as_ref() {
                    Expr::Name(n) => n.id.as_str() == "partial",
                    Expr::Attribute(a) => a.attr.as_str() == "partial",
                    _ => false,
                };
                if is_partial
                    && let Some(first_arg) = inner_call.arguments.args.first()
                    && let Expr::Name(func_name) = first_arg
                {
                    refs.push(NodeRef::FunctionName(func_name.id.to_string()));
                }
            }
        }
        // Plain variable: *work_items — can't extract refs
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trivial_asset() {
        let src = r#"
from barca import asset

@asset()
def single_asset() -> dict:
    return {"status": "ok"}
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, NodeKind::Asset);
        assert_eq!(nodes[0].function_name, "single_asset");
        assert_eq!(nodes[0].freshness, Freshness::Always);
    }

    #[test]
    fn env_list_is_read_statically() {
        let src = r#"
from barca import asset, task

@asset(env=["SOURCE_CSV", "API_TOKEN", "SOURCE_CSV"])
def raw() -> dict:
    return {}

@task(env=["DEPLOY_TARGET"])
def deploy(raw):
    pass

@asset()
def plain() -> int:
    return 1
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].env, vec!["SOURCE_CSV", "API_TOKEN"]);
        assert_eq!(nodes[1].env, vec!["DEPLOY_TARGET"]);
        assert!(nodes[2].env.is_empty());
    }

    #[test]
    fn env_must_be_a_literal_list_of_strings() {
        for decl in [
            "env=NAMES",
            "env=(\"A\",)",
            "env=\"A\"",
            "env=[\"A\", NAME]",
            "env=[\"A\", 1]",
            "env=[\"\"]",
            "env=[\"A=B\"]",
            "env=[f\"{X}\"]",
        ] {
            let src =
                format!("from barca import asset\n\n@asset({decl})\ndef a():\n    return 1\n");
            let err = extract_nodes(&src, "test.py").expect_err(decl).to_string();
            assert!(err.contains("test.py: a: invalid env="), "{decl}: {err}");
            assert!(err.contains("env=[\"SOURCE_CSV\""), "{decl}: {err}");
        }
    }

    #[test]
    fn test_bare_asset() {
        let src = r#"
from barca import asset

@asset
def bare() -> dict:
    return {}
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].function_name, "bare");
    }

    #[test]
    fn test_inputs() {
        let src = r#"
from barca import asset

@asset()
def a() -> str:
    return "hello"

@asset(inputs={"a": a})
def b(a: str) -> str:
    return a.upper()
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].inputs.len(), 1);
        assert_eq!(nodes[1].inputs[0].param_name, "a");
        assert!(!nodes[1].inputs[0].collected);
    }

    #[test]
    fn test_sensor_and_task() {
        let src = r#"
from barca import sensor, task, Schedule, Always

@sensor(freshness=Schedule("*/5 * * * *"))
def my_sensor():
    return (True, {})

@task(inputs={"data": my_sensor}, freshness=Always())
def my_task(data):
    print(data)
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].kind, NodeKind::Sensor);
        assert_eq!(
            nodes[0].freshness,
            Freshness::Schedule(CronExpr("*/5 * * * *".into()))
        );
        assert_eq!(nodes[1].kind, NodeKind::Task);
    }

    #[test]
    fn type_annotations_extracted_from_signature() {
        use crate::model::ValueType;

        let src = r#"
from barca import asset

@asset()
def raw() -> pl.DataFrame:
    ...

@asset(inputs={"orders": raw, "meta": raw})
def stg(orders: pl.DataFrame, meta: pd.DataFrame) -> pd.DataFrame:
    ...

@asset(inputs={"parts": raw})
def collected(parts: list[pl.DataFrame]) -> pl.DataFrame:
    ...
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes.len(), 3);

        assert_eq!(nodes[0].return_type, Some(ValueType::Polars));
        assert!(nodes[0].param_types.is_empty());

        assert_eq!(nodes[1].return_type, Some(ValueType::Pandas));
        assert_eq!(nodes[1].param_types.get("orders"), Some(&ValueType::Polars));
        assert_eq!(nodes[1].param_types.get("meta"), Some(&ValueType::Pandas));

        assert_eq!(nodes[2].return_type, Some(ValueType::Polars));
        assert_eq!(nodes[2].param_types.get("parts"), Some(&ValueType::Polars));
    }

    #[test]
    fn lazyframe_annotation_is_its_own_value_type() {
        use crate::model::ValueType;

        let src = r#"
from barca import asset

@asset()
def raw() -> pl.LazyFrame:
    ...

@asset(inputs={"orders": raw, "eager": raw})
def stg(orders: pl.LazyFrame, eager: polars.DataFrame) -> dict:
    ...

@asset(inputs={"parts": raw})
def collected(parts: list[polars.LazyFrame]) -> dict:
    ...
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].return_type, Some(ValueType::PolarsLazy));
        assert_eq!(
            nodes[1].param_types.get("orders"),
            Some(&ValueType::PolarsLazy)
        );
        assert_eq!(nodes[1].param_types.get("eager"), Some(&ValueType::Polars));
        assert_eq!(
            nodes[2].param_types.get("parts"),
            Some(&ValueType::PolarsLazy)
        );
        assert_eq!(ValueType::PolarsLazy.as_str(), "polars_lazy");
        assert_eq!(
            serde_json::to_value(ValueType::PolarsLazy).unwrap(),
            "polars_lazy"
        );
    }

    #[test]
    fn test_sink_stacking() {
        let src = r#"
from barca import asset, sink

@asset()
@sink("tmp/out.json", serializer="json")
@sink("s3://bucket/out.txt", serializer="text")
def my_asset() -> dict:
    return {}
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].sinks.len(), 2);
        assert_eq!(nodes[0].sinks[0].path, "tmp/out.json");
        assert_eq!(nodes[0].sinks[0].serializer, Some(SerializerKind::Json));
        assert_eq!(nodes[0].sinks[1].path, "s3://bucket/out.txt");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Parallel call extraction
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parallel_static_partials_extracted() {
        let src = r#"
@task()
def deploy_all():
    parallel(partial(deploy_us, model), partial(deploy_eu, model))
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 1);
        assert!(!nodes[0].parallel_calls[0].is_dynamic);
        assert_eq!(nodes[0].parallel_calls[0].static_refs.len(), 2);
    }

    #[test]
    fn parallel_dynamic_generator_extracted() {
        let src = r#"
@task()
def deploy_all():
    parallel(*(partial(deploy, r) for r in regions))
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 1);
        assert!(nodes[0].parallel_calls[0].is_dynamic);
        assert_eq!(nodes[0].parallel_calls[0].static_refs.len(), 1); // knows the function
    }

    #[test]
    fn parallel_map_extracted() {
        let src = r#"
@task()
def deploy_all():
    parallel_map(deploy, regions)
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 1);
        assert!(nodes[0].parallel_calls[0].is_dynamic);
        assert_eq!(nodes[0].parallel_calls[0].static_refs.len(), 1);
    }

    #[test]
    fn parallel_fully_dynamic_extracted() {
        let src = r#"
@task()
def release():
    parallel(*work_items)
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 1);
        assert!(nodes[0].parallel_calls[0].is_dynamic);
        assert!(nodes[0].parallel_calls[0].static_refs.is_empty());
    }

    #[test]
    fn parallel_not_extracted_from_assets() {
        let src = r#"
@asset()
def compute():
    parallel(partial(a), partial(b))
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert!(nodes[0].parallel_calls.is_empty());
    }

    #[test]
    fn parallel_multiple_calls_in_body() {
        let src = r#"
@task()
def pipeline():
    parallel(partial(a), partial(b))
    parallel(partial(c), partial(d))
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 2);
    }

    #[test]
    fn parallel_inside_if_else() {
        let src = r#"
@task()
def conditional():
    if True:
        parallel(partial(a), partial(b))
"#;
        let nodes = extract_nodes(src, "test.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 1);
    }
    #[test]
    fn qualified_and_aliased_nodes_helpers_are_extracted_consistently() {
        for (imports, prefix) in [
            ("import barca", "barca."),
            ("import barca as b", "b."),
            (
                "from barca import asset as a, sensor as s, task as t, sink as sk, unsafe as u, Schedule as S, partitions as p, partitions_from as pf, collect as c, asset_ref as ar",
                "",
            ),
        ] {
            let name = |canonical: &str, alias: &str| {
                if prefix.is_empty() {
                    alias.to_string()
                } else {
                    format!("{prefix}{canonical}")
                }
            };
            let source = format!(
                "{imports}\n@{}()\ndef up(): return 1\n@{}(\"out.json\")\n@{}\n@{}(inputs={{\"x\": {}(up), \"y\": {}(\"other.py:ref\")}}, partitions={{\"key\": {}([\"a\"]), \"derived\": {}(up)}}, freshness={}(\"0 5 * * *\"))\ndef rows(x, y, key, derived): return x\n@{}()\ndef finish(): pass\n",
                name("sensor", "s"),
                name("sink", "sk"),
                name("unsafe", "u"),
                name("asset", "a"),
                name("collect", "c"),
                name("asset_ref", "ar"),
                name("partitions", "p"),
                name("partitions_from", "pf"),
                name("Schedule", "S"),
                name("task", "t")
            );
            let nodes = extract_nodes(&source, "pipeline.py").unwrap();
            assert_eq!(nodes.len(), 3, "{imports}");
            let rows = &nodes[1];
            assert_eq!(rows.kind, NodeKind::Asset);
            assert!(rows.is_unsafe);
            assert_eq!(rows.sinks.len(), 1);
            assert!(rows.inputs[0].collected);
            assert_eq!(
                rows.inputs[1].upstream,
                NodeRef::Canonical("other.py:ref".into())
            );
            assert_eq!(rows.partitions.len(), 2);
            assert!(matches!(rows.freshness, Freshness::Schedule(_)));
        }
    }

    #[test]
    fn explicit_foreign_and_shadowed_decorators_are_not_nodes() {
        for source in [
            "from foreign import asset\n@asset()\ndef f(): pass\n",
            "from barca import duckdb_connection as asset\n@asset()\ndef f(): pass\n",
            "from barca import task as a\nfrom barca import asset as a\n@a()\ndef f(): pass\n",
            "import foreign as b\n@b.asset()\ndef f(): pass\n",
            "from barca import asset as a\na = foreign\n@a()\ndef f(): pass\n",
            "import barca as b\nb = foreign\n@b.asset()\ndef f(): pass\n",
            "import barca as b\nb.asset = foreign\n@b.asset()\ndef f(): pass\n",
            "from barca import asset\nfrom foreign import *\n@asset()\ndef f(): pass\n",
        ] {
            assert!(
                extract_nodes(source, "pipeline.py").unwrap().is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn shared_module_export_writes_block_affected_helpers_and_aliases() {
        let source = "import barca as b\nimport barca as c\nfrom barca import partitions as old_p\nb.partitions = foreign\nfrom barca import partitions as new_p\n@b.asset(partitions={\"a\": c.partitions([1], custom=True), \"b\": new_p([2], custom=True), \"c\": old_p([3], custom=True)})\ndef f(): return 1\n";
        let nodes = extract_nodes(source, "pipeline.py").unwrap();
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].partitions.is_empty());
    }

    #[test]
    fn aliased_decorator_and_helper_arguments_receive_validation() {
        for source in [
            "import barca as b\n@b.asset(input={})\ndef f(): pass\n",
            "from barca import asset as a\n@a(after=other)\ndef f(): pass\n",
            "import barca\n@barca.asset(partitions={\"p\": barca.partitions(values=[1])})\ndef f(p): pass\n",
        ] {
            assert!(
                matches!(
                    extract_nodes(source, "pipeline.py"),
                    Err(ParseError::InvalidArguments { .. })
                ),
                "{source}"
            );
        }
    }

    #[test]
    fn task_helpers_resolve_aliases_without_local_shadowing() {
        let source = "import barca as b\nfrom barca import parallel as p\n@b.task()\ndef f():\n    b.parallel_map(work, items)\n    p(work)\n";
        let nodes = extract_nodes(source, "pipeline.py").unwrap();
        assert_eq!(nodes[0].parallel_calls.len(), 2);
        for body in [
            "def f(b):\n    b.parallel_map(work, items)",
            "def f(p):\n    p(work)",
            "def f():\n    p(work)\n    p = foreign",
        ] {
            let source =
                format!("import barca as b\nfrom barca import parallel as p\n@b.task()\n{body}\n");
            let nodes = extract_nodes(&source, "pipeline.py").unwrap();
            assert!(nodes[0].parallel_calls.is_empty(), "{body}");
        }
    }
}
