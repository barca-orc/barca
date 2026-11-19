//! Preparing a run: the plan, the targets it computes and those blocked on a Manual node, the
//! existing cache and the pins the run holds.

use super::*;
use std::collections::HashSet;

pub(super) struct Prepared {
    pub(super) user_pins: Vec<db::AssetPin>,
    pub(super) dag: std::sync::Arc<Dag>,
    pub(super) targets: Vec<(String, String)>,
    pub(super) job_name: String,
    pub(super) keep_going: bool,
    pub(super) target_label: Option<String>,
    pub(super) pool_size: usize,
    /// The capacity the run's steps share (#436).
    pub(super) capacity: usize,
    pub(super) exec_plan: ExecutionPlan,
    pub(super) plan_warnings: Vec<crate::warnings::PlanWarning>,
    /// `Manual` nodes read as they are (RFC-0008), by id.
    pub(super) pinned: HashMap<String, crate::freshness::Pin>,
    /// Under a tick: the targets that read a `Manual` node that has never been computed.
    pub(super) blocked: Vec<(String, TargetOutcome)>,
    /// Under a tick: the paused nodes (`crate::pauses`); a blocked target whose `failed_node`
    /// is one of them was skipped because it is paused.
    pub(super) paused: HashSet<String>,
    /// `--partition`: what the selection narrowed and skipped.
    pub(super) narrowing: Option<crate::selection::Narrowing>,
}

/// The step reports of a tick's targets blocked on a `Manual` node that has never been
/// computed: `skipped`, reason `blocked`, the detail naming the `Manual` node.
/// A target skipped because it (or a never-computed node it reads) is paused has reason `paused`.
pub(super) fn blocked_steps(
    dag: &Dag,
    blocked: &[(String, TargetOutcome)],
    paused: &HashSet<String>,
) -> Vec<StepReport> {
    blocked
        .iter()
        .map(|(id, outcome)| StepReport {
            id: id.clone(),
            kind: kind_str(dag.get_node(id).map(|n| n.kind())),
            status: Some(StepStatus::Skipped.to_string()),
            reason: Some(
                if outcome
                    .failed_node
                    .as_ref()
                    .is_some_and(|n| paused.contains(n))
                {
                    "paused".to_string()
                } else {
                    StepStatus::Blocked.to_string()
                },
            ),
            detail: outcome.error.clone(),
            ..Default::default()
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_run(
    supplied_dag: Option<std::sync::Arc<Dag>>,
    request: &ExecuteRequest<'_>,
    trace: impl Fn(&str),
) -> Result<Prepared, BarcaError> {
    let (target_names, file_args, python, policy, command_label, partition) = (
        request.target_names,
        request.file_args,
        request.python,
        &request.policy,
        request.command_label,
        request.partition.as_ref(),
    );
    let dag = match supplied_dag {
        Some(dag) => dag,
        None => std::sync::Arc::new(build_dag(file_args, python).await?),
    };
    trace("dag_built");

    // A tick's targets are its scheduled node and the `Always` nodes below it, of any kind.
    let resolve_as = if request.rules.tick.is_some() {
        ""
    } else {
        command_label
    };
    let mut targets = resolve_targets(&dag, target_names, resolve_as)?;
    let Planned {
        user_pins,
        cone,
        pinned,
        blocked,
        paused,
    } = pin_manual(
        &dag,
        &mut targets,
        request.cfg,
        policy,
        &request.rules,
        file_args,
    )
    .await?;
    let cone: Option<Vec<String>> = cone.map(|c| c.into_iter().map(str::to_string).collect());
    let target_ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    // A tick reports as its scheduled node's job, as a skipped tick does.
    let job_name = match &request.rules.tick {
        Some(scheduled) => scheduled.clone(),
        None => canonical_job(&target_ids),
    };
    if command_label == "get"
        && target_ids.is_empty()
        && let Some(note) = skipped_tasks_note(&dag, file_args)
    {
        crate::run_line!(Always, "{note}");
    }
    // Several targets: a step failure stops only what depends on it, so every target that can
    // still run does (one run reports every failure). One target keeps the stop-at-first-failure
    // behavior: nothing else in its cone could produce its value.
    let keep_going = targets.len() > 1;
    // For the run record: the targets as given, comma-separated.
    let target_label: Option<String> = (!targets.is_empty()).then(|| {
        targets
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    });

    let sizing = sizing_for(request.cfg, request.options.host.as_ref());
    let pool_size = sizing.pool_size;
    let config = ResourceConfig {
        pool_size,
        concurrency_groups: HashMap::new(),
    };
    let mut exec_plan = match &cone {
        Some(cone) => {
            let cone: Vec<&str> = cone.iter().map(String::as_str).collect();
            crate::planner::plan_for_nodes(&dag, &cone, &config, partition)
        }
        None => plan_for_targets(&dag, &target_ids, &config, command_label, partition),
    };
    let narrowing = match partition {
        Some(selection) => Some(selection.apply(&dag, &mut exec_plan, &target_ids)?),
        None => None,
    };
    crate::pins::prune_partition_inputs(&dag, &mut exec_plan, &target_ids, &user_pins);
    trace("planned");

    if let CachePolicy::RefreshSelective { names, .. } = policy {
        let planned: Option<Vec<&str>> = cone
            .as_ref()
            .map(|c| c.iter().map(String::as_str).collect());
        validate_refresh_names(
            &dag,
            &target_ids,
            names,
            command_label == "get",
            planned.as_deref(),
        )?;
        if let Some(n) = &narrowing {
            n.check_refresh(names)?;
        }
    }
    if let Some(note) = narrowing.as_ref().and_then(|n| n.note()) {
        crate::run_line!(Always, "{note}");
    }
    // A planned step whose file imports a helper that does not parse cannot run (#550).
    dag.check_imports(exec_plan.step_ids())?;
    // Plan-time warnings for the steps this command planned, before anything runs.
    let plan_warnings = crate::warnings::for_plan(&dag, &exec_plan);
    crate::warnings::print(&plan_warnings);
    let capacity = sizing.capacity;
    warn_over_capacity(
        &planned_weights(&exec_plan),
        capacity,
        sizing.capacity_source,
    );
    if let Some(shared) = request.options.host.shared_capacity() {
        warn_over_capacity(
            &planned_weights(&exec_plan),
            shared.total as usize,
            shared.source,
        );
    }

    Ok(Prepared {
        user_pins,
        dag,
        targets,
        job_name,
        keep_going,
        target_label,
        pool_size,
        capacity,
        exec_plan,
        plan_warnings,
        pinned,
        blocked,
        paused,
        narrowing,
    })
}
