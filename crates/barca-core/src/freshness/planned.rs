//! What a request plans before anything is decided: the `Manual` and pinned nodes it reads as
//! they are (RFC-0008), the paused nodes a tick skips, and the cone that leaves. A run
//! (`crate::execution`) and a prediction of one (`crate::predict`) plan through the same
//! function.

use super::*;
use crate::BarcaError;
use crate::results::TargetOutcome;
use crate::step_status::StepStatus;

/// The metadata DB for a read, or `None` when there is none yet, or it is an empty file
/// (nothing was computed).
async fn existing_cache(
    cfg: &crate::config::ResolvedConfig,
) -> Result<Option<crate::cache::CacheReader>, BarcaError> {
    // An empty file is a database the run is about to initialize: nothing was computed.
    if std::fs::metadata(&cfg.db_path).is_ok_and(|m| m.len() > 0) {
        Ok(Some(crate::cache::CacheReader::for_config(cfg).await?))
    } else {
        Ok(None)
    }
}

/// The `Manual` nodes a request for `targets` reads as they are (RFC-0008), and what is planned:
/// the cone, not walking past them. A `Manual` node that has never been computed is a usage
/// error; under a tick it blocks the targets that read it instead, which are moved to the
/// returned list. Stale ones get a note on stderr (not under a tick).
pub(crate) struct Planned<'a> {
    pub(crate) user_pins: Vec<db::AssetPin>,
    pub(crate) cone: Option<Vec<&'a str>>,
    pub(crate) pinned: HashMap<String, crate::freshness::Pin>,
    pub(crate) blocked: Vec<(String, TargetOutcome)>,
    pub(crate) paused: HashSet<String>,
}

/// The outcome of a tick's target that is not run because `node` is paused.
fn paused_outcome(node: &str, error: String) -> TargetOutcome {
    TargetOutcome {
        status: StepStatus::Blocked.to_string(),
        final_output: None,
        error: Some(error),
        failed_node: Some(node.to_string()),
        failed_phase: None,
        partitions: None,
        key_summary: None,
    }
}

pub(crate) async fn pin_manual<'a>(
    dag: &'a Dag,
    targets: &mut Vec<(String, String)>,
    cfg: &crate::config::ResolvedConfig,
    policy: &CachePolicy,
    rules: &crate::freshness::RunRules,
    file_args: &[String],
) -> Result<Planned<'a>, BarcaError> {
    let mut planned = Planned {
        user_pins: Vec::new(),
        cone: None,
        pinned: HashMap::new(),
        blocked: Vec::new(),
        paused: HashSet::new(),
    };
    // The status prediction plans every node and holds none: what reads a held node is given
    // the result a run reads as each one is decided (`crate::cache::read_as_held`).
    if targets.is_empty() || rules.status {
        return Ok(planned);
    }
    // A tick does not compute a paused node (`crate::pauses`): a paused target is skipped.
    if rules.tick.is_some() {
        planned.paused = db::get_asset_pauses(&cfg.db_path)
            .await?
            .into_iter()
            .map(|p| p.id)
            .collect();
        let paused = &planned.paused;
        targets.retain(|(name, id)| {
            if paused.contains(id) {
                planned
                    .blocked
                    .push((name.clone(), paused_outcome(id, crate::pauses::detail(id))));
                false
            } else {
                true
            }
        });
        if targets.is_empty() {
            return Ok(planned);
        }
    }
    let cache = existing_cache(cfg).await?;
    // Assets pinned with `barca pin`: read as their pinned materialization by every run.
    let user = match &cache {
        Some(cache) => db::pins_on(cache.conn()).await?,
        None => Default::default(),
    };
    let user_ids: HashSet<String> = user.keys().cloned().collect();
    planned.user_pins = user.values().cloned().collect();
    if !rules.pin_manual && user.is_empty() {
        return Ok(planned);
    }
    let ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let walk = crate::freshness::walk_with(
        dag,
        &ids,
        policy,
        &user_ids,
        rules.pin_manual,
        &planned.paused,
    );
    if walk.stops.is_empty() {
        return Ok(planned);
    }
    let pins = crate::freshness::load_pins(cache.as_ref(), dag, &walk.stops, &user).await;
    drop(cache);
    if let Some(first) = pins.lost.first()
        && rules.tick.is_none()
    {
        return Err(BarcaError::Usage(crate::freshness::pin_lost(first)));
    }
    if let Some(first) = pins.never.first()
        && rules.tick.is_none()
    {
        return Err(crate::freshness::never_computed(dag, first, file_args));
    }
    if !pins.never.is_empty() || !pins.lost.is_empty() {
        targets.retain(|(name, id)| {
            let stops = crate::freshness::walk_with(
                dag,
                &[id],
                policy,
                &user_ids,
                rules.pin_manual,
                &planned.paused,
            )
            .stops;
            let (node, error) = if let Some(lost) = stops.iter().find(|s| pins.lost.contains(s)) {
                (
                    lost.clone(),
                    format!(
                        "blocked on '{}', pinned to a materialization that can no longer be read",
                        short_name(lost)
                    ),
                )
            } else if let Some(held) = stops
                .iter()
                .find(|s| pins.never.contains(s) && planned.paused.contains(*s))
            {
                (
                    held.clone(),
                    format!(
                        "blocked on '{}', a paused node that has never been computed",
                        short_name(held)
                    ),
                )
            } else if let Some(manual) = stops.iter().find(|s| pins.never.contains(s)) {
                (
                    manual.clone(),
                    format!(
                        "blocked on '{}', a Manual node that has never been computed",
                        short_name(manual)
                    ),
                )
            } else {
                return true;
            };
            planned.blocked.push((
                name.clone(),
                TargetOutcome {
                    status: StepStatus::Blocked.to_string(),
                    final_output: None,
                    error: Some(error),
                    failed_node: Some(node),
                    failed_phase: None,
                    partitions: None,
                    key_summary: None,
                },
            ));
            false
        });
    }
    let ids: Vec<&str> = targets.iter().map(|(_, id)| id.as_str()).collect();
    let mut cone = crate::freshness::walk_with(
        dag,
        &ids,
        policy,
        &user_ids,
        rules.pin_manual,
        &planned.paused,
    )
    .cone;
    // Outside a tick, the sensors a pinned node reads directly are polled too, so the stale note
    // compares against what they return now, not what they returned last time.
    if rules.tick.is_none() {
        let mut planned_ids: std::collections::HashSet<&str> = cone.iter().copied().collect();
        for id in pins
            .pinned
            .iter()
            .filter(|(_, pin)| pin.user.is_none())
            .map(|(id, _)| id)
        {
            for up in dag.upstream(id) {
                if dag
                    .get_node(up)
                    .is_some_and(|n| n.kind() == crate::NodeKind::Sensor)
                {
                    planned_ids.insert(up);
                }
            }
        }
        cone = dag
            .topo_order()
            .into_iter()
            .filter(|id| planned_ids.contains(id))
            .collect();
    }
    planned.cone = Some(cone);
    planned.pinned = pins.pinned;
    Ok(planned)
}
