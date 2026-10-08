//! Input validation, project roots, and output selection.

use crate::{
    args::{Cli, OutputMode},
    error::{self, CliError, Context, ErrorKind, shell_quote},
    output::{self, Format, FormatFlags},
};
use std::path::{Path, PathBuf};

pub(crate) fn list_hint(files: &[PathBuf]) -> String {
    let files: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    error::list_hint(&files)
}

/// A `get`/`run` usage error (exit 2), ending with the `barca list` pointer.
pub(crate) fn usage_error(msg: &str, files: &[PathBuf]) -> CliError {
    CliError::from_prose(ErrorKind::Usage, format!("{msg}\n\n{}", list_hint(files)))
}

/// Usage line for `barca get` / `barca run`.
pub(crate) fn usage_line(sub: &str) -> &'static str {
    match sub {
        "run" => RUN_USAGE,
        "status" => STATUS_USAGE,
        _ => GET_USAGE,
    }
}

/// The cache policy of `get` / `run`: one vocabulary on both (`--refresh a,b`, `--no-cascade`,
/// `--refresh-all`). `--no-cache` is the deprecated spelling of `--refresh-all`.
pub(crate) fn cache_policy(
    refresh: Option<Vec<String>>,
    no_cascade: bool,
    refresh_all: bool,
    no_cache: bool,
) -> barca_core::cache::CachePolicy {
    use barca_core::cache::CachePolicy;
    if no_cache {
        eprintln!(
            "[barca] warning: --no-cache is deprecated and will be removed in a future minor \
             release; use --refresh-all"
        );
    }
    match (refresh_all || no_cache, refresh) {
        (true, _) => CachePolicy::RefreshAll,
        (false, Some(names)) => CachePolicy::RefreshSelective {
            names,
            cascade: !no_cascade,
        },
        (false, None) => CachePolicy::CacheAware,
    }
}

/// Positionals in the wrong order: the first ends in `.py` and a later one does not. With
/// exactly one such name there is one valid reading, so the error prints the corrected command
/// (same files and flags, target first). With several it states the rule without guessing.
/// `raw` is the command line after the program name, used to carry the flags over.
pub(crate) fn wrong_order_error(sub: &str, args: &[String], raw: &[String]) -> Option<String> {
    let first = args.first()?;
    if !first.ends_with(".py") {
        return None;
    }
    let targets: Vec<&String> = args.iter().filter(|a| !a.ends_with(".py")).collect();
    let files: Vec<PathBuf> = args
        .iter()
        .filter(|a| a.ends_with(".py"))
        .map(PathBuf::from)
        .collect();
    let hint = list_hint(&files);
    match targets.as_slice() {
        [] => None,
        [target] => {
            // Flags: the raw command line minus the subcommand and the positionals, in order.
            let mut rest = raw;
            if rest.first().map(String::as_str) == Some(sub) {
                rest = &rest[1..];
            }
            let mut pending = args.iter().peekable();
            let flags: Vec<String> = rest
                .iter()
                .filter(|tok| {
                    if pending.peek() == Some(tok) {
                        pending.next();
                        false
                    } else {
                        true
                    }
                })
                .map(|t| shell_quote(t))
                .collect();
            let mut cmd = vec!["barca".to_string(), sub.to_string(), shell_quote(target)];
            cmd.extend(files.iter().map(|f| shell_quote(&f.to_string_lossy())));
            cmd.extend(flags);
            Some(format!(
                "error: the target comes before the files\n\n  {}\n\n{hint}",
                cmd.join(" ")
            ))
        }
        several => {
            let names: Vec<String> = several.iter().map(|t| format!("'{t}'")).collect();
            Some(format!(
                "error: the target comes before the files (found {} after '{first}')\n\n{}\n\n{hint}",
                names.join(", "),
                usage_line(sub)
            ))
        }
    }
}

/// A usage error (exit 2) with the corrected command when the positionals are in the wrong
/// order.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
pub(crate) fn check_order(sub: &str, args: &[String]) -> Result<(), CliError> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    match wrong_order_error(sub, args, &raw) {
        Some(msg) => Err(CliError::from_prose(ErrorKind::Usage, msg)),
        None => Ok(()),
    }
}

/// Reject file arguments that are not `.py` files, with a hint for the most common mistake:
/// passing several assets to `--refresh` separated by spaces instead of commas.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
pub(crate) fn check_py_files(
    files: &[PathBuf],
    refresh: Option<&[String]>,
) -> Result<(), CliError> {
    let Some(bad) = files.iter().find(|f| !f.to_string_lossy().ends_with(".py")) else {
        return Ok(());
    };
    let bad = bad.to_string_lossy();
    let mut msg = format!("error: '{bad}' is not a .py file.");
    if let Some(names) = refresh {
        let mut all: Vec<String> = names.to_vec();
        all.push(bad.to_string());
        msg.push_str(&format!(
            "\n\nIf you meant to refresh several assets, join them with commas: --refresh {}",
            all.join(",")
        ));
    }
    let py: Vec<PathBuf> = files
        .iter()
        .filter(|f| f.to_string_lossy().ends_with(".py"))
        .cloned()
        .collect();
    Err(usage_error(&msg, &py))
}

pub(crate) const GET_USAGE: &str =
    "Usage: barca get [TARGET] <FILES>... [--refresh a,b [--no-cascade] | --refresh-all]";
pub(crate) const STATUS_USAGE: &str = "Usage: barca status [TARGET] <FILES>...";
pub(crate) const RUN_USAGE: &str =
    "Usage: barca run <TARGET> <FILES>... [--refresh a,b | --refresh-all]";

/// Errors from a `get`/`run` that are the caller's mistake (unknown target, task/asset misuse,
/// unknown `--refresh` name) end with the `barca list` pointer, like every other get/run usage
/// error.
pub(crate) fn get_run_error(
    e: barca_core::BarcaError,
    ctx: &Context,
    files: &[PathBuf],
) -> CliError {
    let usage = matches!(
        e,
        barca_core::BarcaError::Usage(_) | barca_core::BarcaError::AssetNotFound(..)
    );
    let err = CliError::from_barca(e, ctx);
    if usage {
        err.with_final_hint(list_hint(files))
    } else {
        err
    }
}

/// Whether this invocation's output mode is JSON, which makes errors a JSON envelope on
/// stderr (see error.rs). It follows the same rule as results (output.rs): get/run/list/
/// history/stats print JSON when a flag or BARCA_OUTPUT says so, or when stdout is not a
/// terminal; `plan` is always JSON; `docs` only with `--json`.
pub(crate) fn json_output(cli: &Cli) -> bool {
    match cli {
        Cli::Get {
            output,
            format,
            fields,
            ..
        }
        | Cli::Run {
            output,
            format,
            fields,
            ..
        } => get_run_mode(*output, *format, fields.as_deref())
            .is_ok_and(|m| matches!(m, OutputMode::Json)),
        Cli::Plan { .. } => true,
        Cli::History { format, fields, .. }
        | Cli::Stats { format, fields, .. }
        | Cli::List { format, fields, .. }
        | Cli::Status { format, fields, .. } => {
            fields_json(*format, fields.as_deref()).unwrap_or(false)
        }
        Cli::Docs { json, fields, .. } => *json || fields.is_some(),
        Cli::Sql { format, .. } => is_json(*format),
        Cli::Serve { .. } | Cli::Version => false,
    }
}

/// The command name and files of an invocation, for remediation hints.
pub(crate) fn context(cli: &Cli) -> Context {
    let paths = |files: &[PathBuf]| files.iter().map(|p| p.display().to_string()).collect();
    let (command, files) = match cli {
        Cli::Get { args, .. } => ("get", paths(&split_target_files(args.clone()).1)),
        Cli::Run { args, .. } => ("run", paths(&split_target_files(args.clone()).1)),
        Cli::Plan { files, .. } => ("plan", paths(files)),
        Cli::Stats { files, .. } => ("stats", paths(files)),
        Cli::Serve { files, .. } => ("serve", paths(files)),
        Cli::List { files, .. } => ("list", paths(files)),
        Cli::Sql { files, .. } => ("sql", paths(files)),
        Cli::Status { args, .. } => ("status", paths(&split_target_files(args.clone()).1)),
        Cli::History { .. } => ("history", Vec::new()),
        Cli::Docs { .. } => ("docs", Vec::new()),
        Cli::Version => ("version", Vec::new()),
    };
    Context { command, files }
}

/// The get/run output mode. `--fields` trims JSON, so it implies JSON; combined with an
/// explicit human mode (`-o value`, `-o pretty`, `--pretty`) it is a usage error.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
pub(crate) fn get_run_mode(
    output: Option<OutputMode>,
    format: FormatFlags,
    fields: Option<&[String]>,
) -> Result<OutputMode, CliError> {
    if fields.is_none() {
        return Ok(OutputMode::resolve(output, format));
    }
    match (output, format.pretty) {
        (Some(OutputMode::Value | OutputMode::Pretty), _) | (_, true) => Err(CliError::from_prose(
            ErrorKind::Usage,
            "error: --fields applies to JSON output\nDrop -o/--pretty, or use --json.",
        )),
        _ => Ok(OutputMode::Json),
    }
}

/// Whether an inspection command prints JSON. `--fields` implies JSON; with `--pretty` it is a
/// usage error.
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
pub(crate) fn fields_json(
    format: FormatFlags,
    fields: Option<&[String]>,
) -> Result<bool, CliError> {
    match (fields, format.pretty) {
        (Some(_), true) => Err(CliError::from_prose(
            ErrorKind::Usage,
            "error: --fields applies to JSON output\nDrop --pretty, or use --json.",
        )),
        (Some(_), false) => Ok(true),
        (None, _) => Ok(is_json(format)),
    }
}

/// Apply `--fields` to `barca docs` JSON: each `topics[]` entry, or the single topic object.
pub(crate) fn project_docs_json(out: String, fields: Option<&[String]>) -> String {
    let Some(fields) = fields else { return out };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&out) else {
        return out;
    };
    if v.get("topics").is_some() {
        barca_core::report::project_key(&mut v, "topics", Some(fields));
    } else {
        barca_core::report::project_one(&mut v, fields);
    }
    serde_json::to_string_pretty(&v).unwrap_or_default() + "\n"
}

/// Change into the project root (the nearest barca.toml at or above the cwd, else the cwd) and
/// turn the file arguments into the project's file list: paths are rebased onto the root, and
/// directories, or no files at all, are expanded by tree discovery (`barca_core::discover`).
/// A note on stderr names the root when it is not the cwd.
pub(crate) fn enter_project_root(cli: &mut Cli) -> Result<(), barca_core::BarcaError> {
    use barca_core::BarcaError;
    let cwd = std::env::current_dir()
        .map_err(|e| BarcaError::Other(format!("cannot determine cwd: {e}")))?;
    let root = barca_core::config::find_root(&cwd).unwrap_or_else(|| cwd.clone());
    if root != cwd {
        std::env::set_current_dir(&root).map_err(|e| {
            BarcaError::Other(format!(
                "cannot change into project root {}: {e}",
                root.display()
            ))
        })?;
        eprintln!(
            "barca: project root: {} ({} found above the cwd)",
            root.display(),
            barca_core::config::CONFIG_FILE
        );
    }
    let rebase = |p: &Path| barca_core::config::rebase_onto_root(p, &cwd, &root);
    // Discovery needs [discovery] from barca.toml; an invalid file is reported here once.
    let mut discovery: Option<barca_core::config::DiscoveryToml> = None;
    let mut expand = |files: Vec<PathBuf>| -> Result<Vec<PathBuf>, BarcaError> {
        let files: Vec<PathBuf> = files.iter().map(|f| rebase(f)).collect();
        // A stray non-path argument (`--refresh a b`) is left for the usage checks to explain.
        if files
            .iter()
            .any(|f| !f.to_string_lossy().ends_with(".py") && !f.is_dir())
        {
            return Ok(files);
        }
        let walk = files.is_empty() || files.iter().any(|f| f.is_dir());
        if !walk {
            return Ok(files);
        }
        if discovery.is_none() {
            discovery = Some(
                barca_core::config::load_toml(Path::new("."))?
                    .and_then(|t| t.discovery)
                    .unwrap_or_default(),
            );
        }
        let cfg = discovery.as_ref().expect("loaded above");
        Ok(barca_core::discover::discover(Path::new("."), &files, cfg)?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    };
    match cli {
        Cli::Get { args, .. } | Cli::Run { args, .. } | Cli::Status { args, .. } => {
            let (target, files) = split_target_files(std::mem::take(args));
            // A target with stray words after it (wrong order) is left for check_order.
            let files = if files.iter().all(|f| is_path_arg(&f.to_string_lossy())) {
                expand(files)?
            } else {
                files
            };
            args.extend(target);
            args.extend(files.iter().map(|f| f.to_string_lossy().into_owned()));
        }
        Cli::Plan { files, .. }
        | Cli::Stats { files, .. }
        | Cli::Serve { files, .. }
        | Cli::Sql { files, .. }
        | Cli::List { files, .. } => {
            *files = expand(std::mem::take(files))?;
        }
        Cli::History { .. } | Cli::Docs { .. } | Cli::Version => {}
    }
    Ok(())
}

/// Whether a positional names files rather than a target: a `.py` file, a path written with a
/// trailing `/`, `.` or `..`, or an existing directory written with a `/` in it. A bare word is
/// always a target, even when a directory has that name (write `name/` for the directory).
pub(crate) fn is_path_arg(arg: &str) -> bool {
    arg.ends_with(".py")
        || arg.ends_with('/')
        || arg == "."
        || arg == ".."
        || (arg.contains('/') && !arg.contains(':') && Path::new(arg).is_dir())
}

/// Split the raw positional args into (optional target, files).
/// If the first arg names files (`is_path_arg`), all args are files (no target).
/// Otherwise, the first arg is the target and the rest are files.
pub(crate) fn split_target_files(args: Vec<String>) -> (Option<String>, Vec<PathBuf>) {
    if args.is_empty() {
        return (None, Vec::new());
    }
    if is_path_arg(&args[0]) {
        // All args are files.
        let files = args.into_iter().map(PathBuf::from).collect();
        (None, files)
    } else {
        // First arg is the target, rest are files.
        let target = args[0].clone();
        let files = args[1..].iter().map(PathBuf::from).collect();
        (Some(target), files)
    }
}

/// Split a target argument into its comma-separated names (`a,b` -> `[a, b]`), dropping
/// repeats. An empty name (`a,,b`, `a,`) is a usage error.
pub(crate) fn parse_targets(raw: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for name in raw.split(',') {
        if name.is_empty() {
            return Err(format!(
                "error: empty target name in '{raw}'\n\n\
                 Separate several targets with single commas, no spaces: barca run a,b pipeline.py"
            ));
        }
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    Ok(out)
}

/// The target list of `get`/`run` (empty when no target was given); a malformed list is a
/// usage error (exit 2).
#[allow(clippy::result_large_err)] // cold path: built once, right before exiting
pub(crate) fn targets_arg(
    target: Option<&str>,
    files: &[PathBuf],
) -> Result<Vec<String>, CliError> {
    match target.map(parse_targets) {
        None => Ok(Vec::new()),
        Some(Ok(names)) => Ok(names),
        Some(Err(msg)) => Err(usage_error(&msg, files)),
    }
}

/// Inspection commands use the shared output rule.
pub(crate) fn is_json(flags: FormatFlags) -> bool {
    output::resolve(flags.explicit()) == Format::Json
}
