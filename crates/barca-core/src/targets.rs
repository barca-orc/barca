//! Target resolution, cone selection, and per-target outcomes.
use crate::dispatch;
use crate::planner::{self, ExecutionPlan, ResourceConfig};
use crate::queries::filter_plan_to_subgraph;
use crate::recover;
use crate::{BarcaError, dag::Dag, dispatch::OutputRef, results::TargetOutcome};
use std::collections::HashMap;

/// Does a target `name` identify node `id`? A name is a function name (`deploy`), a full id
/// (`pipeline.py:deploy`), or a path-suffixed id (`p.py:deploy` for `sub/p.py:deploy`). It
/// matches only at a `:` or `/` boundary, never as the tail of a longer name, so `deploy` does
/// not select `prod_deploy`.
pub(crate) fn target_name_matches(id: &str, name: &str) -> bool {
    if id == name {
        return true;
    }
    match id.strip_suffix(name) {
        Some(prefix) => prefix.ends_with(':') || (name.contains(':') && prefix.ends_with('/')),
        None => false,
    }
}

/// Why a name cannot be the target of a command. The one place that decides it: `barca get`,
/// `barca run` and the server's `POST /get/{target}` and `POST /run/{target}` all go through
/// [`resolve_target_among`], so they refuse the same names with the same words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetError {
    /// No node has this name. `available` is every node id, in topological order.
    NotFound {
        name: String,
        available: Vec<String>,
    },
    /// Several nodes have it (the same function name in two files).
    Ambiguous { name: String, matches: Vec<String> },
    /// One node has it, of a kind the command does not take: `get` is for assets and
    /// sensors, `run` for tasks and sensors.
    WrongKind { name: String, kind: crate::NodeKind },
}

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TargetError::NotFound { name, available } => {
                write!(
                    f,
                    "Asset '{name}' not found. Available: {}",
                    available.join(", ")
                )
            }
            TargetError::Ambiguous { name, matches } => write!(
                f,
                "'{name}' matches more than one node: {}. Name one by its full id, e.g. `{}`",
                matches.join(", "),
                matches[0]
            ),
            TargetError::WrongKind { name, kind } => match kind {
                crate::NodeKind::Task => write!(f, "'{name}' is a task — use `barca run` instead"),
                _ => write!(f, "'{name}' is an asset — use `barca get` instead"),
            },
        }
    }
}

impl From<TargetError> for BarcaError {
    fn from(e: TargetError) -> Self {
        match e {
            TargetError::NotFound { name, available } => {
                BarcaError::AssetNotFound(name, available.join(", "))
            }
            other => BarcaError::Usage(other.to_string()),
        }
    }
}

/// The single node `name` identifies among `nodes` (id and kind, in topological order), as a
/// target of `command_label` (`get`, `run`; anything else takes every kind).
pub fn resolve_target_among<'a>(
    nodes: impl IntoIterator<Item = (&'a str, crate::NodeKind)>,
    name: &str,
    command_label: &str,
) -> Result<String, TargetError> {
    let nodes: Vec<(&str, crate::NodeKind)> = nodes.into_iter().collect();
    let matches: Vec<&(&str, crate::NodeKind)> = nodes
        .iter()
        .filter(|(id, _)| target_name_matches(id, name))
        .collect();
    let ids = |of: &[&(&str, crate::NodeKind)]| of.iter().map(|(id, _)| id.to_string()).collect();
    match matches.as_slice() {
        [] => Err(TargetError::NotFound {
            name: name.to_string(),
            available: nodes.iter().map(|(id, _)| id.to_string()).collect(),
        }),
        [(id, kind)] => {
            // `barca get` is for assets, `barca run` is for tasks.
            let wrong = (command_label == "get" && *kind == crate::NodeKind::Task)
                || (command_label == "run" && *kind == crate::NodeKind::Asset);
            if wrong {
                return Err(TargetError::WrongKind {
                    name: name.to_string(),
                    kind: *kind,
                });
            }
            Ok(id.to_string())
        }
        many => Err(TargetError::Ambiguous {
            name: name.to_string(),
            matches: ids(many),
        }),
    }
}

/// The DAG's nodes as [`resolve_target_among`] takes them.
fn dag_nodes(dag: &Dag) -> impl Iterator<Item = (&str, crate::NodeKind)> {
    dag.topo_order()
        .into_iter()
        .filter_map(|id| dag.get_node(id).map(|node| (id, node.kind())))
}

/// The single node a target name identifies, whatever its kind. No match is `AssetNotFound`;
/// several matches (the same function name in two files) is a usage error that lists the full
/// ids to choose from.
pub(crate) fn find_target_id(dag: &Dag, name: &str) -> Result<String, BarcaError> {
    Ok(resolve_target_among(dag_nodes(dag), name, "")?)
}

/// The node a command's target names, checked for its kind: `barca get`
/// targets assets and `barca run` targets tasks.
pub(crate) fn resolve_target(
    dag: &Dag,
    target_name: Option<&str>,
    command_label: &str,
) -> Result<Option<String>, BarcaError> {
    match target_name {
        Some(name) => Ok(Some(resolve_target_among(
            dag_nodes(dag),
            name,
            command_label,
        )?)),
        None => Ok(None),
    }
}

/// Resolve several target names (`barca run a,b`) to node ids, each checked like a single
/// target, before anything runs. Returns `(name as given, node id)` pairs; a name resolving to an
/// id already listed is dropped, so `a,a` is one target.
pub(crate) fn resolve_targets(
    dag: &Dag,
    names: &[String],
    command_label: &str,
) -> Result<Vec<(String, String)>, BarcaError> {
    let mut out: Vec<(String, String)> = Vec::new();
    for name in names {
        if let Some(id) = resolve_target(dag, Some(name), command_label)?
            && !out.iter().any(|(_, seen)| *seen == id)
        {
            out.push((name.clone(), id));
        }
    }
    Ok(out)
}

/// The plan for these targets: the union of their cones, planned once, so an upstream step
/// shared by several targets appears (and runs) once. No targets means everything the command
/// covers: for `get`, every asset and sensor (tasks are skipped: get is for assets, run is for
/// tasks); for anything else (`status`), the whole DAG.
pub(crate) fn plan_for_targets(
    dag: &Dag,
    target_ids: &[&str],
    config: &ResourceConfig,
    command_label: &str,
) -> ExecutionPlan {
    let full_plan = planner::plan_from_dag(dag, config);
    if !target_ids.is_empty() {
        filter_plan_to_subgraph(full_plan, &dag.subgraph_many(target_ids))
    } else if command_label == "get" {
        // A task is never upstream of an asset or sensor, so this is closed under upstream.
        let gettable: Vec<&str> = dag
            .topo_order()
            .into_iter()
            .filter(|id| !is_task(dag, id))
            .collect();
        filter_plan_to_subgraph(full_plan, &gettable)
    } else {
        full_plan
    }
}

pub(crate) fn is_task(dag: &Dag, id: &str) -> bool {
    dag.get_node(id)
        .is_some_and(|n| n.kind() == crate::NodeKind::Task)
}

/// The stderr note for `barca get <files>` with no target when the files define tasks: which
/// tasks were skipped and how to run one. `None` when there is no task to mention.
pub(crate) fn skipped_tasks_note(dag: &Dag, file_args: &[String]) -> Option<String> {
    let order = dag.topo_order();
    let tasks: Vec<&str> = order
        .iter()
        .filter(|id| is_task(dag, id))
        .map(|id| short_name(id))
        .collect();
    let first = tasks.first()?;
    let run_hint = format!("barca run {first} {}", file_args.join(" "));
    let listed = tasks.join(", ");
    Some(if tasks.len() == order.len() {
        format!(
            "[barca] nothing to get: no assets or sensors, only tasks ({listed}). \
             `barca get` without a target never runs tasks; run one with: {run_hint}"
        )
    } else {
        format!(
            "[barca] skipped {} task{} ({listed}): `barca get` without a target materializes \
             assets only. Run a task with: {run_hint}",
            tasks.len(),
            if tasks.len() == 1 { "" } else { "s" },
        )
    })
}

/// The failed node upstream of `base_id`, if any: it blocks `base_id` from running. Used when
/// several targets run together, so a failure stops only the targets that need it.
pub(crate) fn blocking_failure<'a>(
    dag: &'a Dag,
    base_id: &str,
    failed: &std::collections::HashSet<String>,
) -> Option<&'a str> {
    if failed.is_empty() {
        return None;
    }
    dag.subgraph(base_id)
        .into_iter()
        .find(|up| *up != base_id && failed.contains(*up))
}

/// The output of `target_id` in this run (the first partition, by key, for a partitioned one).
pub(crate) fn output_for(
    target_id: &str,
    all_outputs: &HashMap<String, OutputRef>,
) -> Option<OutputRef> {
    all_outputs.get(target_id).cloned().or_else(|| {
        let prefix = format!("{target_id}[");
        let mut matches: Vec<_> = all_outputs
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .collect();
        matches.sort_by_key(|(k, _)| (*k).clone());
        matches.first().map(|(_, v)| (*v).clone())
    })
}

/// How each target of a multi-target run ended: `success` with its output, or `failed` with
/// the step that failed (the target itself or something upstream of it) and its error.
pub(crate) fn target_outcomes(
    dag: &Dag,
    targets: &[(String, String)],
    all_outputs: &HashMap<String, OutputRef>,
    failures: &[dispatch::StepFailure],
) -> Vec<(String, TargetOutcome)> {
    let base = |f: &dispatch::StepFailure| crate::StepId::parse(&f.node_id).base_id().to_string();
    targets
        .iter()
        .map(|(name, tid)| {
            // Prefer the target's own failure, then the first failed step upstream of it.
            let failure = failures.iter().find(|f| base(f) == *tid).or_else(|| {
                dag.subgraph(tid)
                    .into_iter()
                    .find_map(|id| failures.iter().find(|f| base(f) == id))
            });
            let outcome = match (failure, output_for(tid, all_outputs)) {
                (Some(f), _) => TargetOutcome {
                    status: "failed".to_string(),
                    final_output: None,
                    error: Some(f.error.message.clone()),
                    failed_node: Some(f.node_id.clone()),
                },
                (None, Some(out)) => TargetOutcome {
                    status: "success".to_string(),
                    final_output: Some(out),
                    error: None,
                    failed_node: None,
                },
                (None, None) => TargetOutcome {
                    status: "failed".to_string(),
                    final_output: None,
                    error: Some("did not run".to_string()),
                    failed_node: None,
                },
            };
            (name.clone(), outcome)
        })
        .collect()
}

/// Does a `--refresh` name (a function name, or a full `file.py:name` id) identify `node_id`?
pub(crate) fn refresh_name_matches(node_id: &str, name: &str) -> bool {
    node_id == name || node_id.ends_with(&format!(":{name}"))
}

/// The function name of a node id (`pipeline.py:src` -> `src`, `pipeline.py:p[k=v]` -> `p`).
pub(crate) fn short_name(node_id: &str) -> &str {
    let base = node_id.split('[').next().unwrap_or(node_id);
    base.rsplit(':').next().unwrap_or(base)
}

/// Fail before running anything when `--refresh` names something that is not an upstream
/// asset of the target: a typo must not be a silent no-op. `get` targets are assets, so with
/// `include_targets` a target may name itself; `run` targets are tasks and never can.
pub(crate) fn validate_refresh_names(
    dag: &Dag,
    target_ids: &[&str],
    names: &[String],
    include_targets: bool,
) -> Result<(), BarcaError> {
    let cone: Vec<&str> = if target_ids.is_empty() {
        dag.topo_order()
    } else {
        dag.subgraph_many(target_ids)
    };
    let assets: Vec<&str> = cone
        .into_iter()
        .filter(|id| include_targets || !target_ids.contains(id))
        .filter(|id| {
            dag.get_node(id)
                .is_some_and(|n| n.kind() == crate::NodeKind::Asset)
        })
        .collect();
    for name in names {
        if !assets.iter().any(|id| refresh_name_matches(id, name)) {
            let valid: Vec<&str> = assets.iter().map(|id| short_name(id)).collect();
            return Err(BarcaError::Usage(format!(
                "--refresh: no upstream asset named '{name}'{}.\n\
                 Upstream assets you can refresh: {}\n\
                 Pass several as a comma-separated list: --refresh {}",
                match target_ids {
                    [] => String::new(),
                    [t] => format!(" in the cone of '{}'", short_name(t)),
                    many => format!(
                        " in the cones of {}",
                        many.iter()
                            .map(|t| format!("'{}'", short_name(t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
                if valid.is_empty() {
                    "(none)".to_string()
                } else {
                    valid.join(", ")
                },
                valid.iter().take(2).copied().collect::<Vec<_>>().join(","),
            )));
        }
    }
    Ok(())
}

/// The output a run returns as `final_output`: the target's when there is exactly one (for a
/// partitioned target, the first partition by key), none when there are several (each target
/// reports its own), and with no target the last planned asset's (a sensor's only when the plan
/// has no asset).
pub(crate) fn final_output_of(
    exec_plan: &ExecutionPlan,
    target_ids: &[&str],
    several_targets: bool,
    all_outputs: &HashMap<String, OutputRef>,
) -> Option<OutputRef> {
    if let [tid] = target_ids {
        return output_for(tid, all_outputs);
    }
    if several_targets {
        return None;
    }
    let last_planned_id = recover::returned_step(exec_plan)
        .map(|s| s.step_id.display())
        .unwrap_or_default();
    all_outputs.get(&last_planned_id).cloned().or_else(|| {
        let mut matches: Vec<_> = all_outputs
            .iter()
            .filter(|(k, _)| k.starts_with(&last_planned_id))
            .collect();
        matches.sort_by_key(|(k, _)| (*k).clone());
        matches.first().map(|(_, v)| (*v).clone())
    })
}

#[cfg(test)]
mod refresh_name_tests {
    use super::refresh_name_matches;

    #[test]
    fn matches_the_full_id_or_the_function_name() {
        assert!(refresh_name_matches("pipeline.py:src", "src"));
        assert!(refresh_name_matches("pipeline.py:src", "pipeline.py:src"));
        assert!(!refresh_name_matches("pipeline.py:src", "rc"));
        assert!(!refresh_name_matches("pipeline.py:source", "src"));
        assert!(!refresh_name_matches("pipeline.py:src", "nope"));
    }
}

#[cfg(test)]
mod multi_target_tests {
    use super::*;
    use crate::planner::ResourceConfig;
    use crate::report::{RunOutcome, end_of_run_line, failed_step_line};
    use crate::results::{
        ExplainResult, ExplainSummary, MultiResult, PartitionSummary, StepReport, TargetPrediction,
    };

    const SRC: &str = r#"
from barca import asset, task


@asset()
def src() -> int:
    return 1


@asset(inputs={"s": src})
def left(s: int) -> int:
    return s


@asset(inputs={"l": left})
def deeper(l: int) -> int:
    return l


@task(inputs={"s": src})
def check_a(s: int) -> None:
    pass


@task(inputs={"d": deeper})
def check_b(d: int) -> None:
    pass


@asset()
def lone() -> int:
    return 2
"#;

    fn dag() -> Dag {
        let nodes = crate::parse::extract_nodes(SRC, "p.py").unwrap();
        Dag::build(&nodes).unwrap()
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn failure(node_id: &str, message: &str) -> dispatch::StepFailure {
        dispatch::StepFailure {
            node_id: node_id.to_string(),
            error: dispatch::StepError {
                error_type: "WorkerError".to_string(),
                message: message.to_string(),
                traceback: String::new(),
                attempts: 1,
            },
        }
    }

    fn oref(path: &str) -> OutputRef {
        OutputRef {
            path: path.to_string(),
            format: "json".to_string(),
            size_bytes: 1,
            elapsed_seconds: None,
            content_hash: None,
        }
    }

    fn outcome(status: &str) -> TargetOutcome {
        TargetOutcome {
            status: status.to_string(),
            final_output: None,
            error: None,
            failed_node: None,
        }
    }

    #[test]
    fn every_name_resolves_and_repeats_collapse() {
        let dag = dag();
        let got = resolve_targets(&dag, &names(&["check_a", "check_b", "check_a"]), "run").unwrap();
        assert_eq!(
            got,
            vec![
                ("check_a".to_string(), "p.py:check_a".to_string()),
                ("check_b".to_string(), "p.py:check_b".to_string()),
            ]
        );
        assert!(resolve_targets(&dag, &[], "get").unwrap().is_empty());
    }

    #[test]
    fn one_bad_name_fails_the_whole_list() {
        let dag = dag();
        let err = resolve_targets(&dag, &names(&["check_a", "nope"]), "run").unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
        let err = resolve_targets(&dag, &names(&["left", "check_a"]), "get").unwrap_err();
        assert!(err.to_string().contains("barca run"), "{err}");
    }

    #[test]
    fn the_plan_is_the_union_of_cones_with_shared_upstream_once() {
        let dag = dag();
        let config = ResourceConfig {
            pool_size: 4,
            concurrency_groups: HashMap::new(),
        };
        let plan = plan_for_targets(&dag, &["p.py:check_a", "p.py:check_b"], &config, "run");
        let ids: Vec<String> = plan
            .phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.display())
            .collect();
        assert_eq!(ids.iter().filter(|i| *i == "p.py:src").count(), 1);
        for want in ["p.py:left", "p.py:deeper", "p.py:check_a", "p.py:check_b"] {
            assert!(ids.iter().any(|i| i == want), "{want} missing from {ids:?}");
        }
        assert!(!ids.iter().any(|i| i == "p.py:lone"));
    }

    fn planned_ids(plan: &ExecutionPlan) -> Vec<String> {
        plan.phases
            .iter()
            .flat_map(|p| &p.streams)
            .flat_map(|s| &s.steps)
            .map(|s| s.step_id.display())
            .collect()
    }

    #[test]
    fn bare_get_plans_every_asset_and_no_task() {
        let dag = dag();
        let config = ResourceConfig {
            pool_size: 4,
            concurrency_groups: HashMap::new(),
        };
        let mut got = planned_ids(&plan_for_targets(&dag, &[], &config, "get"));
        got.sort();
        assert_eq!(got, ["p.py:deeper", "p.py:left", "p.py:lone", "p.py:src"]);
        // status (inspection) still covers the whole file, tasks included.
        let all = planned_ids(&plan_for_targets(&dag, &[], &config, "status"));
        assert!(all.iter().any(|i| i == "p.py:check_a"), "{all:?}");
    }

    #[test]
    fn skipped_tasks_note_names_the_tasks_and_the_run_command() {
        let files = names(&["p.py"]);
        let note = skipped_tasks_note(&dag(), &files).unwrap();
        assert!(
            note.contains("skipped 2 tasks (check_a, check_b)"),
            "{note}"
        );
        assert!(note.contains("barca run check_a p.py"), "{note}");

        let only = "from barca import task\n\n@task()\ndef deploy() -> None:\n    pass\n";
        let only = Dag::build(&crate::parse::extract_nodes(only, "t.py").unwrap()).unwrap();
        let note = skipped_tasks_note(&only, &names(&["t.py"])).unwrap();
        assert!(note.contains("nothing to get"), "{note}");
        assert!(note.contains("barca run deploy t.py"), "{note}");

        let assets = "from barca import asset\n\n@asset()\ndef a() -> int:\n    return 1\n";
        let assets = Dag::build(&crate::parse::extract_nodes(assets, "a.py").unwrap()).unwrap();
        assert!(skipped_tasks_note(&assets, &names(&["a.py"])).is_none());
    }

    #[test]
    fn refresh_names_may_come_from_any_targets_cone() {
        let dag = dag();
        let both = ["p.py:check_a", "p.py:check_b"];
        validate_refresh_names(&dag, &both, &names(&["src", "deeper"]), false).unwrap();
        let msg = validate_refresh_names(&dag, &both, &names(&["lone"]), false)
            .unwrap_err()
            .to_string();
        assert!(
            msg.contains("in the cones of 'check_a', 'check_b'"),
            "{msg}"
        );
        assert!(
            validate_refresh_names(&dag, &["p.py:check_a"], &names(&["deeper"]), false).is_err()
        );
    }

    #[test]
    fn a_failure_blocks_only_what_depends_on_it() {
        let dag = dag();
        let failed: std::collections::HashSet<String> = ["p.py:left".to_string()].into();
        assert_eq!(
            blocking_failure(&dag, "p.py:check_b", &failed),
            Some("p.py:left")
        );
        assert_eq!(blocking_failure(&dag, "p.py:check_a", &failed), None);
        assert_eq!(blocking_failure(&dag, "p.py:left", &failed), None);
    }

    #[test]
    fn each_target_reports_its_own_outcome() {
        let dag = dag();
        let targets = vec![
            ("check_a".to_string(), "p.py:check_a".to_string()),
            ("check_b".to_string(), "p.py:check_b".to_string()),
            ("lone".to_string(), "p.py:lone".to_string()),
        ];
        let outputs: HashMap<String, OutputRef> =
            [("p.py:check_a".to_string(), oref("a.json"))].into();
        let failures = vec![failure("p.py:left", "boom")];
        let out = target_outcomes(&dag, &targets, &outputs, &failures);
        assert_eq!(out[0].0, "check_a");
        assert_eq!(out[0].1.status, "success");
        assert_eq!(out[0].1.final_output.as_ref().unwrap().path, "a.json");
        assert_eq!(out[1].1.status, "failed");
        assert_eq!(out[1].1.failed_node.as_deref(), Some("p.py:left"));
        assert_eq!(out[1].1.error.as_deref(), Some("boom"));
        assert_eq!(out[2].1.status, "failed");
        assert_eq!(out[2].1.error.as_deref(), Some("did not run"));
    }

    #[test]
    fn end_of_run_line_is_one_format_and_never_says_done_on_failure() {
        assert_eq!(
            end_of_run_line(3, 3, 1.26, RunOutcome::Done),
            "[barca] 3/3 steps | done in 1.3s"
        );
        let failed = end_of_run_line(0, 3, 0.0, RunOutcome::Failed);
        assert_eq!(failed, "[barca] 0/3 steps | failed in 0.0s");
        assert!(!failed.contains("done"));
        assert_eq!(
            end_of_run_line(1, 3, 2.0, RunOutcome::Cancelled),
            "[barca] 1/3 steps | cancelled after 2.0s"
        );
    }

    #[test]
    fn failed_step_line_names_the_step_and_the_first_error_line() {
        assert_eq!(
            failed_step_line(
                "p.py:broken",
                "\nValueError: boom\n  File \"p.py\", line 3, in broken"
            ),
            "[barca] step:p.py:broken failed: ValueError: boom"
        );
        assert_eq!(
            failed_step_line("p.py:x", ""),
            "[barca] step:p.py:x failed: unknown error"
        );
    }

    #[test]
    fn summary_add_counts_like_the_dry_run() {
        let line = |action: &str, p: Option<(usize, usize)>| StepReport {
            action: Some(action.to_string()),
            partitions: p.map(|(cached, will_run)| PartitionSummary {
                total: cached + will_run,
                cached,
                will_run,
                will_run_keys: Vec::new(),
            }),
            ..Default::default()
        };
        let mut s = ExplainSummary::default();
        for r in [
            line("cached", None),
            line("run", None),
            line("unknown", None),
            line("partial", Some((2, 3))),
            line("run", Some((0, 4))),
        ] {
            s.add(&r);
        }
        assert_eq!(
            s,
            ExplainSummary {
                will_run: 8,
                cached: 3,
                unknown: 1
            }
        );
    }

    #[test]
    fn dry_run_json_names_one_target_or_lists_several() {
        let mut r = ExplainResult {
            dry_run: true,
            command: "run".to_string(),
            target: Some("a".to_string()),
            targets: Vec::new(),
            steps: Vec::new(),
            summary: ExplainSummary::default(),
            warnings: Vec::new(),
        };
        let one = serde_json::to_value(&r).unwrap();
        assert_eq!(one["target"], "a");
        assert!(one.get("targets").is_none());
        r.target = None;
        let predicted = |will_run| TargetPrediction {
            summary: ExplainSummary {
                will_run,
                cached: 1,
                unknown: 0,
            },
        };
        r.targets = vec![
            ("b".to_string(), predicted(2)),
            ("a".to_string(), predicted(0)),
        ];
        assert_eq!(r.target_names(), names(&["b", "a"]));
        let many = serde_json::to_value(&r).unwrap();
        assert_eq!(
            many["targets"],
            serde_json::json!({
                "b": {"summary": {"will_run": 2, "cached": 1, "unknown": 0}},
                "a": {"summary": {"will_run": 0, "cached": 1, "unknown": 0}},
            })
        );
        assert!(many.get("target").is_none());
        // Keyed in the order given, like a real multi-target run.
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.find("\"b\"").unwrap() < s.find("\"a\"").unwrap(), "{s}");
    }

    #[test]
    fn multi_result_targets_serialize_as_a_map_in_the_order_given() {
        let r = MultiResult {
            run_id: "r".to_string(),
            elapsed_seconds: 0.0,
            steps_executed: 0,
            phases: 0,
            steps: Vec::new(),
            warnings: Vec::new(),
            targets: vec![
                ("zeta".to_string(), outcome("success")),
                ("alpha".to_string(), outcome("failed")),
            ],
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(
            s.find("\"zeta\"").unwrap() < s.find("\"alpha\"").unwrap(),
            "{s}"
        );
        assert!(r.any_failed());
    }
}

#[cfg(test)]
mod target_name_tests {
    use super::target_name_matches;

    #[test]
    fn exact_names_and_ids_match() {
        assert!(target_name_matches("p.py:deploy", "deploy"));
        assert!(target_name_matches("p.py:deploy", "p.py:deploy"));
        assert!(target_name_matches("sub/p.py:deploy", "p.py:deploy"));
        assert!(target_name_matches("sub/p.py:deploy", "sub/p.py:deploy"));
    }

    #[test]
    fn a_suffix_of_another_name_does_not_match() {
        assert!(!target_name_matches("p.py:prod_deploy", "deploy"));
        assert!(!target_name_matches("p.py:dyn_margin_all", "margin_all"));
        assert!(!target_name_matches("subp.py:deploy", "p.py:deploy"));
        assert!(!target_name_matches("sub/p.py:deploy", "/p.py:deploy_x"));
    }
}

#[cfg(test)]
mod resolve_target_tests {
    use super::{TargetError, resolve_target_among};
    use crate::{BarcaError, NodeKind};

    const NODES: [(&str, NodeKind); 5] = [
        ("a.py:orders", NodeKind::Asset),
        ("a.py:poll", NodeKind::Sensor),
        ("a.py:publish", NodeKind::Task),
        ("b.py:orders", NodeKind::Asset),
        ("b.py:total", NodeKind::Asset),
    ];

    fn resolve(name: &str, command: &str) -> Result<String, TargetError> {
        resolve_target_among(NODES, name, command)
    }

    #[test]
    fn a_name_that_identifies_one_node_of_a_kind_the_command_takes() {
        assert_eq!(resolve("total", "get").unwrap(), "b.py:total");
        assert_eq!(resolve("b.py:orders", "get").unwrap(), "b.py:orders");
        assert_eq!(resolve("poll", "get").unwrap(), "a.py:poll");
        assert_eq!(resolve("publish", "run").unwrap(), "a.py:publish");
        assert_eq!(resolve("poll", "run").unwrap(), "a.py:poll");
        // A command that is neither (stats, status) takes every kind.
        assert_eq!(resolve("publish", "").unwrap(), "a.py:publish");
    }

    #[test]
    fn an_unknown_name_lists_what_exists() {
        let err = resolve("nope", "get").unwrap_err();
        assert!(matches!(err, TargetError::NotFound { .. }));
        let text = "Asset 'nope' not found. Available: a.py:orders, a.py:poll, a.py:publish, \
                    b.py:orders, b.py:total";
        assert_eq!(err.to_string(), text);
        // The command line's error is the same text.
        let cli = BarcaError::from(err);
        assert!(matches!(cli, BarcaError::AssetNotFound(..)));
        assert_eq!(cli.to_string(), text);
        // A name matches whole, never as the tail of a longer one.
        assert!(matches!(
            resolve("rders", "get"),
            Err(TargetError::NotFound { .. })
        ));
    }

    #[test]
    fn a_name_several_nodes_have_lists_them() {
        let err = resolve("orders", "get").unwrap_err();
        assert!(matches!(err, TargetError::Ambiguous { .. }));
        let text = "'orders' matches more than one node: a.py:orders, b.py:orders. \
                    Name one by its full id, e.g. `a.py:orders`";
        assert_eq!(err.to_string(), text);
        let cli = BarcaError::from(err);
        assert!(matches!(cli, BarcaError::Usage(_)));
        assert_eq!(cli.to_string(), text);
    }

    #[test]
    fn the_wrong_command_for_the_kind_names_the_right_one() {
        let err = resolve("publish", "get").unwrap_err();
        assert!(matches!(
            err,
            TargetError::WrongKind {
                kind: NodeKind::Task,
                ..
            }
        ));
        assert_eq!(
            err.to_string(),
            "'publish' is a task — use `barca run` instead"
        );
        let err = resolve("total", "run").unwrap_err();
        assert_eq!(
            err.to_string(),
            "'total' is an asset — use `barca get` instead"
        );
        assert!(matches!(BarcaError::from(err), BarcaError::Usage(_)));
    }
}
