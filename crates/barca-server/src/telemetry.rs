//! Owned, best-effort lifecycle telemetry. No inspection or run-history writes.
use crate::state::AppState;
use barca_core::{
    CancellationToken,
    telemetry::{self, Integration, ServerPhase, ServerReport},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const HEARTBEAT: Duration = Duration::from_secs(300);

pub(crate) struct ServerTelemetry {
    task: tokio::task::JoinHandle<()>,
}

impl ServerTelemetry {
    pub(crate) fn start(state: AppState, stopping: CancellationToken) -> Option<Self> {
        let integrations = telemetry::configured();
        if integrations.is_empty() {
            return None;
        }
        Some(Self {
            task: tokio::spawn(deliver(state, stopping, integrations)),
        })
    }

    pub(crate) async fn finish(&mut self) {
        // Delivery is already timeout-bounded in core. A total bound also handles
        // future integrations without extending server shutdown indefinitely.
        let _ = tokio::time::timeout(Duration::from_secs(3), &mut self.task).await;
    }
}

impl Drop for ServerTelemetry {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn report(state: &AppState, phase: ServerPhase) -> ServerReport {
    // Never wait for an inspection lock, parse files, or resolve dynamic partitions.
    let nodes = state.loaded_node_count();
    let schedules = state
        .schedule
        .try_read()
        .map(|jobs| jobs.len())
        .unwrap_or(0);
    ServerReport {
        phase,
        start_unix_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u64::MAX as u128) as u64,
        files: state.config.files.len(),
        nodes,
        schedules,
        read_only: state.config.read_only,
        watch: state.config.watch,
        scheduling: state.config.schedule && !state.config.read_only,
    }
}

async fn deliver(
    state: AppState,
    stopping: CancellationToken,
    integrations: Vec<(String, Box<dyn Integration>)>,
) {
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut phase = ServerPhase::Start;
    loop {
        let signal = report(&state, phase);
        tokio::select! {
            biased;
            _ = stopping.cancelled() => break,
            _ = telemetry::export_server(&integrations, &signal) => {}
        }
        tokio::select! {
            biased;
            _ = stopping.cancelled() => break,
            _ = interval.tick() => { phase = ServerPhase::Heartbeat; }
        }
    }
    telemetry::export_server(&integrations, &report(&state, ServerPhase::Stop)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use barca_core::telemetry::RunReport;
    use std::{future::Future, pin::Pin, sync::Arc};
    struct Recorder {
        signals: Arc<std::sync::Mutex<Vec<ServerPhase>>>,
        stall: bool,
    }
    impl Integration for Recorder {
        fn export<'a>(
            &'a self,
            _: &'a RunReport,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async { panic!("lifecycle must not export a run") })
        }
        fn export_server<'a>(
            &'a self,
            signal: &'a ServerReport,
        ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
            Box::pin(async move {
                self.signals.lock().unwrap().push(signal.phase);
                if self.stall {
                    std::future::pending::<()>().await;
                }
                Ok(())
            })
        }
    }
    fn state(dir: &std::path::Path) -> AppState {
        AppState::new(crate::ServeConfig {
            files: vec![dir.join("absent.py").display().to_string()],
            host: std::net::Ipv4Addr::LOCALHOST.into(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "missing-python".into(),
            resolved: barca_core::config::resolve_in(None, dir).unwrap(),
            read_only: true,
        })
    }
    #[tokio::test]
    async fn counts_selected_graph_without_refreshing_or_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let file = &state.config.files[0];
        std::fs::write(
            file,
            "from barca import asset\n@asset\ndef first(): return 1\n@asset\ndef second(first): return first + 1\n",
        )
        .unwrap();
        state.loaded_dag().await.unwrap();
        assert!(state.cache.read().unwrap().assets.is_none());
        assert_eq!(report(&state, ServerPhase::Start).nodes, Some(2));

        // A heartbeat must neither inspect changed sources nor wait for a loader.
        std::fs::write(file, "this is not valid Python !!!").unwrap();
        assert_eq!(report(&state, ServerPhase::Heartbeat).nodes, Some(2));
        let held = state.loaded.lock().await;
        assert_eq!(report(&state, ServerPhase::Heartbeat).nodes, None);
        drop(held);
        assert_eq!(report(&state, ServerPhase::Stop).nodes, Some(2));
        assert!(state.runs.is_empty());
        assert!(!dir.path().join(".barca").exists());
    }

    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }
    #[tokio::test(start_paused = true)]
    async fn idle_heartbeat_then_stop_without_inspection_or_history() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stopping = CancellationToken::new();
        let task = tokio::spawn(deliver(
            state.clone(),
            stopping.clone(),
            vec![(
                "recorder".into(),
                Box::new(Recorder {
                    signals: signals.clone(),
                    stall: false,
                }),
            )],
        ));
        settle().await;
        assert_eq!(*signals.lock().unwrap(), vec![ServerPhase::Start]);
        tokio::time::advance(HEARTBEAT - Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(signals.lock().unwrap().len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(
            *signals.lock().unwrap(),
            vec![ServerPhase::Start, ServerPhase::Heartbeat]
        );
        stopping.cancel();
        task.await.unwrap();
        tokio::time::advance(HEARTBEAT * 2).await;
        settle().await;
        assert_eq!(
            *signals.lock().unwrap(),
            vec![
                ServerPhase::Start,
                ServerPhase::Heartbeat,
                ServerPhase::Stop
            ]
        );
        assert!(state.runs.is_empty());
        assert!(state.cache.read().unwrap().assets.is_none());
        assert!(!dir.path().join(".barca").exists());
    }
    #[tokio::test(start_paused = true)]
    async fn stopping_cancels_stalled_start_and_bounds_stop() {
        let dir = tempfile::tempdir().unwrap();
        let stopping = CancellationToken::new();
        let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let task = tokio::spawn(deliver(
            state(dir.path()),
            stopping.clone(),
            vec![(
                "stalled".into(),
                Box::new(Recorder {
                    signals: signals.clone(),
                    stall: true,
                }),
            )],
        ));
        settle().await;
        stopping.cancel();
        settle().await;
        assert_eq!(
            *signals.lock().unwrap(),
            vec![ServerPhase::Start, ServerPhase::Stop]
        );
        tokio::time::advance(Duration::from_secs(3)).await;
        task.await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn missed_ticks_do_not_create_a_delivery_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stopping = CancellationToken::new();
        let task = tokio::spawn(deliver(
            state(dir.path()),
            stopping.clone(),
            vec![(
                "skip".into(),
                Box::new(Recorder {
                    signals: signals.clone(),
                    stall: false,
                }),
            )],
        ));
        settle().await;
        tokio::time::advance(HEARTBEAT * 10).await;
        settle().await;
        assert_eq!(
            *signals.lock().unwrap(),
            vec![ServerPhase::Start, ServerPhase::Heartbeat]
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(signals.lock().unwrap().len(), 2);
        stopping.cancel();
        task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_owner_aborts_exports() {
        let dir = tempfile::tempdir().unwrap();
        let signals = Arc::new(std::sync::Mutex::new(Vec::new()));
        let owner = ServerTelemetry {
            task: tokio::spawn(deliver(
                state(dir.path()),
                CancellationToken::new(),
                vec![(
                    "drop".into(),
                    Box::new(Recorder {
                        signals: signals.clone(),
                        stall: false,
                    }),
                )],
            )),
        };
        settle().await;
        drop(owner);
        tokio::time::advance(HEARTBEAT * 2).await;
        settle().await;
        assert_eq!(*signals.lock().unwrap(), vec![ServerPhase::Start]);
    }
}
