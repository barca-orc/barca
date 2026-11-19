//! `GET /queue`, `POST /queue/pause` and `POST /queue/resume`.
//!
//! `POST /queue/pause` holds every queued run back (runs in flight go on) until
//! `POST /queue/resume`; the pause is kept in `server_state`, so a restart keeps it.

use crate::error::ApiError;
use crate::queue_view::{QueueView, recorded_view, view_with_starts};
use crate::run_model::RunStatus;
use crate::runs::{dispatch, ensure_db};
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use barca_core::serve::run_queue;

/// `GET /queue` — the runs executing and the runs waiting, in order, and whether the queue is
/// paused.
pub async fn get(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    if state.config.read_only {
        return recorded_view(&state).await.map(Json);
    }
    Ok(Json(view_with_starts(&state).await))
}

/// `POST /queue/pause` — hold every queued run back until `POST /queue/resume`; runs in flight
/// go on, and new runs (and scheduled ticks) still join the queue. Kept across restarts.
/// Pausing a paused queue changes nothing. `403` on a `--read-only` server.
pub async fn pause(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    crate::guards::refuse_if_read_only(&state.config)?;
    if state.runs.queue.paused_at().is_none() {
        let now = chrono::Utc::now().timestamp();
        ensure_db(&state).await?;
        run_queue::set_paused(&state.config.resolved.db_path, Some(now)).await?;
        state.runs.queue.set_paused(Some(now));
        barca_core::errln!(
            "[barca] run queue paused: queued runs wait until it is resumed ({} queued, {} \
             running)",
            state.runs.queue.waiting_ids().len(),
            state
                .runs.registry
                .iter()
                .filter(|r| r.status == RunStatus::Running)
                .count()
        );
    }
    Ok(Json(view_with_starts(&state).await))
}

/// `POST /queue/resume` — let queued runs start again, in order. Resuming a queue that is not
/// paused changes nothing. `403` on a `--read-only` server.
pub async fn resume(State(state): State<AppState>) -> Result<Json<QueueView>, ApiError> {
    crate::guards::refuse_if_read_only(&state.config)?;
    if state.runs.queue.paused_at().is_some() {
        ensure_db(&state).await?;
        run_queue::set_paused(&state.config.resolved.db_path, None).await?;
        state.runs.queue.set_paused(None);
        barca_core::errln!(
            "[barca] run queue resumed ({} queued)",
            state.runs.queue.waiting_ids().len()
        );
        dispatch(&state);
    }
    Ok(Json(view_with_starts(&state).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{load, store};
    use crate::queue_view::{health, view};
    use crate::run_model::RunSpec;
    use crate::runs::{admit, resume_at_startup};
    use barca_core::cache::CachePolicy;
    use barca_core::db;
    use barca_core::outcome::{RunCommand, Trigger};
    use std::collections::{BTreeMap, BTreeSet};

    fn state(dir: &std::path::Path) -> AppState {
        state_with(dir, false)
    }

    fn state_with(dir: &std::path::Path, read_only: bool) -> AppState {
        let mut resolved = barca_core::config::resolve_in(None, dir).unwrap();
        resolved.db_path = dir.join("metadata.db").display().to_string();
        resolved.server_max_runs = Some(1);
        AppState::new(crate::ServeConfig {
            files: vec![dir.join("p.py").display().to_string()],
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "/nonexistent-barca-python".into(),
            resolved,
            read_only,
        })
    }

    #[test]
    fn a_read_only_server_reports_the_recorded_queue_in_health() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let writer = state(dir.path());
            let _ = pause(State(writer.clone())).await.unwrap();
            for target in ["a", "b"] {
                let spec = RunSpec::api(RunCommand::Get, vec![target.into()], vec![]);
                admit(&writer, spec).await.unwrap();
            }
            let reader = state_with(dir.path(), true);
            // Health never waits on a read: the first answer starts one in the background.
            let _ = health(&reader);
            let listed = get(State(reader.clone())).await.unwrap().0;
            assert_eq!(listed.queued.len(), 2);
            let health = serde_json::json!(health(&reader));
            assert_eq!(health["queued"], 2, "/health and /queue agree: {health}");
            assert_eq!(health["paused"], true, "{health}");
            assert_eq!(health["running"], 0, "{health}");
        });
    }

    #[test]
    fn a_restarted_server_resumes_the_queue_in_order_under_the_same_ids_and_keeps_the_pause() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let first = state(dir.path());
            // Paused: every run waits, recorded in the order it was accepted.
            let _ = pause(State(first.clone())).await.unwrap();
            let mut ids = Vec::new();
            for target in ["a", "b", "c"] {
                let spec = RunSpec::api(RunCommand::Get, vec![target.into()], vec![]);
                let admitted = admit(&first, spec).await.unwrap();
                assert_eq!(admitted.status, RunStatus::Pending);
                assert_eq!(admitted.position, Some(ids.len() + 1));
                ids.push(admitted.run_id);
            }
            // One leaves the queue before the restart.
            let gone = first.runs.queue.take(&ids[1]).unwrap();
            assert_eq!(gone.spec.targets, ["b"]);
            run_queue::remove(&first.config.resolved.db_path, &ids[1])
                .await
                .unwrap();
            drop(first);

            let next = state(dir.path());
            resume_at_startup(&next).await.unwrap();
            assert_eq!(
                next.runs.queue.waiting_ids(),
                [ids[0].clone(), ids[2].clone()]
            );
            assert!(next.runs.queue.paused_at().is_some(), "the pause is kept");
            {
                // A `runs` guard held across `dispatch` below deadlocks when the two runs share a
                // shard of the map (#637): read it in a scope of its own.
                let run = next.runs.registry.get(&ids[2]).unwrap();
                assert_eq!(run.status, RunStatus::Pending);
                assert_eq!(run.target.as_deref(), Some("c"));
            }
            assert!(
                next.runs.events.contains_key(&ids[2]),
                "a client can follow it"
            );
            let view = view(&next);
            assert_eq!(view.queued.len(), 2);
            assert_eq!(view.queued[1].position, Some(2));
            assert_eq!(view.max_runs, Some(1));
            // Paused, nothing starts; resumed, the head starts (and fails: no Python here).
            dispatch(&next);
            assert_eq!(next.runs.queue.waiting_ids().len(), 2);
            let _ = resume(State(next.clone())).await.unwrap();
            assert_eq!(
                next.runs.queue.waiting_ids(),
                [ids[2].clone()],
                "one run at a time"
            );
        });
    }

    #[test]
    fn a_requested_run_starts_ahead_of_scheduled_runs_with_a_slot_of_its_own() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let state = state(dir.path());
            let _ = pause(State(state.clone())).await.unwrap();
            let scheduled = |target: &str| RunSpec {
                trigger: Trigger::schedule("p.py:a", 0),
                ..RunSpec::api(RunCommand::Get, vec![target.into()], vec![])
            };
            let busy = admit(&state, scheduled("busy")).await.unwrap().run_id;
            let s1 = admit(&state, scheduled("s1")).await.unwrap().run_id;
            let a1 = admit(
                &state,
                RunSpec::api(RunCommand::Get, vec!["a1".into()], vec![]),
            )
            .await
            .unwrap();
            assert_eq!(a1.position, Some(1), "ahead of the scheduled runs");
            let a2 = admit(
                &state,
                RunSpec::api(RunCommand::Get, vec!["a2".into()], vec![]),
            )
            .await
            .unwrap()
            .run_id;
            // A scheduled run holds the only slot.
            state.runs.queue.take(&busy).unwrap();
            let _held = state.runs.slots.clone().try_acquire_owned().unwrap();
            state.runs.registry.get_mut(&busy).unwrap().status = RunStatus::Running;
            let _ = resume(State(state.clone())).await.unwrap();
            assert_eq!(
                state.runs.queue.waiting_ids(),
                [a2, s1],
                "one requested run takes the extra slot; the next waits, still ahead of cron"
            );
        });
    }

    #[test]
    fn a_run_the_last_server_had_started_is_recorded_interrupted_not_started_again() {
        crate::test_support::bounded(|| async {
            let dir = tempfile::tempdir().unwrap();
            let first = state(dir.path());
            let _ = pause(State(first.clone())).await.unwrap();
            let spec = RunSpec::api(RunCommand::Get, vec!["a".into()], vec![]);
            let id = admit(&first, spec).await.unwrap().run_id;
            let db = first.config.resolved.db_path.clone();
            // The server started it and was killed before it recorded anything.
            run_queue::mark_running(&db, &id).await.unwrap();
            drop(first);
            let next = state(dir.path());
            resume_at_startup(&next).await.unwrap();
            assert!(next.runs.queue.waiting_ids().is_empty());
            assert!(run_queue::list(&db).await.unwrap().is_empty());
            let record = db::get_run(&db, &id).await.unwrap().unwrap();
            assert_eq!(record.status, "error");
        });
    }

    #[test]
    fn queued_exact_keys_do_not_widen_after_reload() {
        let spec = RunSpec {
            partition: Some(
                barca_core::selection::PartitionSelection::parse(
                    r#"[{"day":"d1","region":"east"},{"day":"d2","region":"west"}]"#,
                )
                .unwrap(),
            ),
            ..RunSpec::api(
                RunCommand::Get,
                vec!["result".into()],
                vec!["p.py:result".into()],
            )
        };
        let (back, _) = load(&store(&spec, 1.0)).unwrap();
        assert_eq!(back.partition, spec.partition);
    }

    #[test]
    fn a_stored_run_reads_back_as_it_was_accepted() {
        let mut selection = BTreeMap::new();
        selection.insert(
            "region".to_string(),
            BTreeSet::from(["emea".to_string(), "amer".to_string()]),
        );
        let spec = RunSpec {
            command: RunCommand::Run,
            targets: vec!["report".into()],
            nodes: vec!["p.py:report".into()],
            files: vec!["p.py".into()],
            policy: CachePolicy::RefreshSelective {
                names: vec!["raw".into()],
                cascade: false,
            },
            trigger: Trigger::schedule("p.py:report", 1_760_000_000),
            values: true,
            partition: Some(
                barca_core::selection::PartitionSelection::parse(
                    &serde_json::to_string(&selection).unwrap(),
                )
                .unwrap(),
            ),
            batch: vec!["all".into()],
            rules: barca_core::freshness::RunRules::tick("p.py:report"),
            timeout: None,
            kill_requeues: 0,
        };
        let (back, at) = load(&store(&spec, 12.5)).unwrap();
        assert_eq!(at, 12.5);
        assert_eq!(back.command, spec.command);
        assert_eq!(back.targets, spec.targets);
        assert_eq!(back.nodes, spec.nodes);
        assert_eq!(back.files, spec.files);
        assert!(matches!(
            back.policy,
            CachePolicy::RefreshSelective { ref names, cascade: false } if names == &["raw"]
        ));
        assert_eq!(back.trigger, spec.trigger);
        assert!(back.values);
        assert_eq!(back.partition, spec.partition);
        assert_eq!(back.rules, spec.rules);
        for policy in [CachePolicy::CacheAware, CachePolicy::RefreshAll] {
            let mut spec = RunSpec::api(RunCommand::Get, vec![], vec![]);
            spec.policy = policy.clone();
            let (back, _) = load(&store(&spec, 0.0)).unwrap();
            assert_eq!(format!("{:?}", back.policy), format!("{policy:?}"));
        }
        assert!(load("{\"command\": \"get\"}").is_err());
    }
}

