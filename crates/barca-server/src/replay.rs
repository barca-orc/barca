//! Attempt-scoped serialized event windows. The hub owns eviction; readers hold one frame.
use barca_core::{BarcaError, RunEvent};
use futures::{StreamExt, stream::BoxStream};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
struct Limits {
    events: usize,
    run_bytes: usize,
    total_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            events: 65_536,
            run_bytes: 16 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Clone)]
pub(crate) struct Frame {
    pub sequence: u64,
    epoch: Arc<str>,
    pub data: Arc<str>,
    terminal: bool,
    order: u64,
}
impl Frame {
    pub fn id(&self) -> String {
        format!("{}:{}", self.epoch, self.sequence)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorError {
    Invalid,
    Mismatch,
    Expired,
}
impl CursorError {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_event_cursor",
            Self::Mismatch => "event_cursor_mismatch",
            Self::Expired => "event_cursor_expired",
        }
    }
}
pub(crate) fn parse(text: &str) -> Result<(&str, u64), CursorError> {
    if text.len() > 128 {
        return Err(CursorError::Invalid);
    }
    let (epoch, sequence) = text.split_once(':').ok_or(CursorError::Invalid)?;
    if epoch.len() != 32
        || !epoch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || sequence.starts_with('0')
        || sequence.is_empty()
        || !sequence.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(CursorError::Invalid);
    }
    Ok((epoch, sequence.parse().map_err(|_| CursorError::Invalid)?))
}
struct Window {
    epoch: Arc<str>,
    rows: VecDeque<Frame>,
    last: u64,
    expired_through: u64,
    bytes: usize,
    terminal: bool,
    owner: bool,
    readers: usize,
}
struct Windows {
    limits: Limits,
    windows: HashMap<u64, Window>,
    oldest: BTreeMap<u64, u64>,
    bytes: usize,
    next_window: u64,
    next_order: u64,
}
impl Windows {
    fn pop(&mut self, id: u64) {
        let window = self.windows.get_mut(&id).expect("registered window");
        if let Some(frame) = window.rows.pop_front() {
            window.bytes -= frame.data.len();
            self.bytes -= frame.data.len();
            window.expired_through = window.expired_through.max(frame.sequence);
            self.oldest.remove(&frame.order);
        }
    }
    fn collect(&mut self, id: u64) {
        if self
            .windows
            .get(&id)
            .is_some_and(|w| !w.owner && w.readers == 0)
        {
            let window = self.windows.remove(&id).unwrap();
            self.bytes -= window.bytes;
            for frame in window.rows {
                self.oldest.remove(&frame.order);
            }
        }
    }
}
#[derive(Clone)]
pub(crate) struct ReplayHub {
    inner: Arc<Mutex<Windows>>,
}
impl Default for ReplayHub {
    fn default() -> Self {
        Self::with_limits(Limits::default())
    }
}
impl ReplayHub {
    fn with_limits(limits: Limits) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Windows {
                limits,
                windows: HashMap::new(),
                oldest: BTreeMap::new(),
                bytes: 0,
                next_window: 0,
                next_order: 0,
            })),
        }
    }
    pub fn channel(&self) -> Result<RunChannel, BarcaError> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| BarcaError::Other("could not create an event stream epoch".into()))?;
        let epoch: Arc<str> = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into();
        let mut windows = self.inner.lock().unwrap();
        let id = windows.next_window;
        windows.next_window = windows
            .next_window
            .checked_add(1)
            .expect("event window counter exhausted");
        windows.windows.insert(
            id,
            Window {
                epoch,
                rows: VecDeque::new(),
                last: 0,
                expired_through: 0,
                bytes: 0,
                terminal: false,
                owner: true,
                readers: 0,
            },
        );
        Ok(RunChannel {
            lease: Arc::new(Lease {
                hub: self.clone(),
                id,
                progress: Default::default(),
                emitted: tokio::sync::watch::channel(0).0,
            }),
        })
    }
}
struct Lease {
    progress: Mutex<crate::running::Progress>,
    hub: ReplayHub,
    id: u64,
    emitted: tokio::sync::watch::Sender<u64>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut hub = self.hub.inner.lock().unwrap();
        if let Some(window) = hub.windows.get_mut(&self.id) {
            window.owner = false;
        }
        hub.collect(self.id);
    }
}
#[derive(Clone)]
pub(crate) struct RunChannel {
    lease: Arc<Lease>,
}
impl RunChannel {
    #[cfg(test)]
    pub fn new() -> Self {
        ReplayHub::default().channel().unwrap()
    }
    pub fn bytes(&self) -> usize {
        self.lease.hub.inner.lock().unwrap().windows[&self.lease.id].bytes
    }
    pub fn progress(&self) -> crate::running::ProgressSnapshot {
        self.lease.progress.lock().unwrap().snapshot()
    }
    pub fn emit(&self, event: RunEvent) {
        {
            let mut progress = self.lease.progress.lock().unwrap();
            if event.is_terminal() {
                *progress = Default::default();
            } else {
                progress.observe(&event);
            }
        }
        let data = serde_json::to_string(&event).ok().map(Arc::<str>::from);
        let mut hub = self.lease.hub.inner.lock().unwrap();
        let id = self.lease.id;
        let limits = hub.limits;
        let order = hub.next_order;
        hub.next_order = hub
            .next_order
            .checked_add(1)
            .expect("event order counter exhausted");
        let window = hub.windows.get_mut(&id).expect("live window");
        if window.terminal {
            return;
        }
        window.last = window
            .last
            .checked_add(1)
            .expect("event sequence exhausted");
        let sequence = window.last;
        window.terminal = event.is_terminal();
        if let Some(data) =
            data.filter(|data| data.len() <= limits.run_bytes && data.len() <= limits.total_bytes)
        {
            let size = data.len();
            window.rows.push_back(Frame {
                sequence,
                epoch: window.epoch.clone(),
                data,
                terminal: window.terminal,
                order,
            });
            window.bytes += size;
            hub.bytes += size;
            hub.oldest.insert(order, id);
            while hub.windows[&id].rows.len() > limits.events
                || hub.windows[&id].bytes > limits.run_bytes
            {
                hub.pop(id);
            }
            while hub.bytes > limits.total_bytes {
                let oldest = *hub
                    .oldest
                    .first_key_value()
                    .expect("retained bytes have an owner")
                    .1;
                hub.pop(oldest);
            }
        } else {
            // No cursor before this gap can receive a complete suffix. Retaining its prefix
            // would waste the budget, so discard it as well as the oversized frame.
            while !hub.windows[&id].rows.is_empty() {
                hub.pop(id);
            }
            hub.windows.get_mut(&id).unwrap().expired_through = sequence;
        }
        self.lease.emitted.send_replace(sequence);
    }
    pub fn snapshot(&self) -> Vec<RunEvent> {
        let rows: Vec<_> = self.lease.hub.inner.lock().unwrap().windows[&self.lease.id]
            .rows
            .iter()
            .map(|frame| frame.data.clone())
            .collect();
        rows.iter()
            .map(|data| RunEvent::parse(data).expect("serialized event"))
            .collect()
    }
    pub fn resume(
        &self,
        after: Option<&str>,
    ) -> Result<Option<BoxStream<'static, Frame>>, CursorError> {
        let receiver = self.lease.emitted.subscribe();
        let mut hub = self.lease.hub.inner.lock().unwrap();
        let window = hub.windows.get_mut(&self.lease.id).unwrap();
        let sequence = match after {
            None => 0,
            Some(text) => {
                let (epoch, sequence) = parse(text)?;
                if epoch != window.epoch.as_ref() {
                    return Err(CursorError::Mismatch);
                }
                sequence
            }
        };
        if sequence > window.last {
            return Err(CursorError::Invalid);
        }
        if sequence < window.expired_through {
            return Err(CursorError::Expired);
        }
        if window.terminal && sequence == window.last {
            return Ok(None);
        }
        let next = sequence.checked_add(1).ok_or(CursorError::Invalid)?;
        window.readers += 1;
        let cursor = Reader {
            hub: self.lease.hub.clone(),
            id: self.lease.id,
            next,
            done: false,
            closed: false,
            emitted: receiver,
        };
        Ok(Some(
            futures::stream::unfold(cursor, |mut cursor| async move {
                loop {
                    if cursor.done {
                        return None;
                    }
                    cursor.emitted.borrow_and_update();
                    let frame = {
                        let hub = cursor.hub.inner.lock().unwrap();
                        let window = hub.windows.get(&cursor.id)?;
                        if cursor.next <= window.expired_through {
                            return None;
                        }
                        window.rows.front().and_then(|first| {
                            let index =
                                usize::try_from(cursor.next.checked_sub(first.sequence)?).ok()?;
                            window.rows.get(index).cloned()
                        })
                    };
                    if let Some(frame) = frame {
                        cursor.next = frame.sequence.saturating_add(1);
                        cursor.done = frame.terminal;
                        return Some((frame, cursor));
                    }
                    if cursor.closed {
                        return None;
                    }
                    if cursor.emitted.changed().await.is_err() {
                        cursor.closed = true;
                    }
                }
            })
            .boxed(),
        ))
    }
    // Compatibility for internal consumers/tests; HTTP uses the serialized cursor interface.
    #[cfg(test)]
    pub fn subscribe(&self, from: usize) -> BoxStream<'static, (usize, RunEvent)> {
        let epoch = self.lease.hub.inner.lock().unwrap().windows[&self.lease.id]
            .epoch
            .clone();
        let after = (from > 0).then(|| format!("{epoch}:{from}"));
        self.resume(after.as_deref())
            .ok()
            .flatten()
            .unwrap_or_else(|| futures::stream::empty().boxed())
            .map(|frame| {
                (
                    (frame.sequence - 1) as usize,
                    RunEvent::parse(&frame.data).expect("serialized event"),
                )
            })
            .boxed()
    }
}
struct Reader {
    hub: ReplayHub,
    id: u64,
    next: u64,
    done: bool,
    closed: bool,
    emitted: tokio::sync::watch::Receiver<u64>,
}
impl Drop for Reader {
    fn drop(&mut self) {
        let mut hub = self.hub.inner.lock().unwrap();
        if let Some(window) = hub.windows.get_mut(&self.id) {
            window.readers -= 1;
        }
        hub.collect(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(number: usize) -> RunEvent {
        RunEvent::log(
            "p.py:a".into(),
            number.to_string(),
            barca_core::LogStream::Stdout,
        )
    }
    fn hub(events: usize, run_bytes: usize, total_bytes: usize) -> ReplayHub {
        ReplayHub::with_limits(Limits {
            events,
            run_bytes,
            total_bytes,
        })
    }
    async fn collect(channel: &RunChannel, after: Option<&str>) -> Vec<Frame> {
        channel.resume(after).unwrap().unwrap().collect().await
    }
    fn finish(channel: &RunChannel) {
        channel.emit(RunEvent::RunFinished {
            run_id: "run".into(),
            ok: true,
            outcome: None,
        });
    }

    #[tokio::test]
    async fn cursor_resume_is_strictly_after_and_terminal_acknowledgement_is_empty() {
        let channel = RunChannel::new();
        channel.emit(event(1));
        channel.emit(event(2));
        finish(&channel);
        let frames = collect(&channel, None).await;
        assert_eq!(
            frames.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(parse(&frames[0].id()).unwrap().0.len(), 32);
        let resumed = collect(&channel, Some(&frames[0].id())).await;
        assert_eq!(
            resumed.iter().map(|f| f.sequence).collect::<Vec<_>>(),
            [2, 3]
        );
        assert!(channel.resume(Some(&frames[2].id())).unwrap().is_none());
    }
    #[test]
    fn invalid_overflow_future_and_other_attempt_cursors_are_distinct() {
        let channel = RunChannel::new();
        for text in [
            "0",
            "",
            "ABC:1",
            "00000000000000000000000000000000:0",
            "00000000000000000000000000000000:01",
            "00000000000000000000000000000000:18446744073709551616",
            &"x".repeat(129),
        ] {
            assert!(matches!(
                channel.resume(Some(text)),
                Err(CursorError::Invalid)
            ));
        }
        let epoch = channel.lease.hub.inner.lock().unwrap().windows[&channel.lease.id]
            .epoch
            .clone();
        assert!(matches!(
            channel.resume(Some(&format!("{epoch}:1"))),
            Err(CursorError::Invalid)
        ));
        assert!(matches!(
            channel.resume(Some("00000000000000000000000000000000:1")),
            Err(CursorError::Mismatch)
        ));
    }
    #[tokio::test]
    async fn count_eviction_expires_first_attach_and_lagging_readers_without_queues() {
        let channel = hub(3, 4096, 8192).channel().unwrap();
        channel.emit(event(0));
        let mut slow = channel.resume(None).unwrap().unwrap();
        let first = slow.next().await.unwrap();
        for i in 1..5 {
            channel.emit(event(i));
        }
        finish(&channel);
        assert!(matches!(channel.resume(None), Err(CursorError::Expired)));
        assert!(matches!(
            channel.resume(Some(&first.id())),
            Err(CursorError::Expired)
        ));
        assert!(slow.next().await.is_none());
        let locked = channel.lease.hub.inner.lock().unwrap();
        assert_eq!(locked.windows[&channel.lease.id].rows.len(), 3);
    }
    #[test]
    fn global_budget_evicts_oldest_across_active_windows_and_releases_on_drop() {
        let hub = hub(100, 512, 512);
        let first = hub.channel().unwrap();
        let second = hub.channel().unwrap();
        for i in 0..20 {
            if i % 2 == 0 {
                first.emit(event(i));
            } else {
                second.emit(event(i));
            }
        }
        let state = hub.inner.lock().unwrap();
        assert!(state.bytes <= 512);
        assert!(state.windows[&first.lease.id].expired_through > 0);
        assert!(state.windows[&second.lease.id].expired_through > 0);
        drop(state);
        drop(first);
        drop(second);
        let state = hub.inner.lock().unwrap();
        assert_eq!(state.bytes, 0);
        assert!(state.oldest.is_empty());
        assert!(state.windows.is_empty());
    }
    #[tokio::test]
    async fn oversized_event_creates_a_gap_and_later_events_can_resume_after_it() {
        let channel = hub(10, 256, 512).channel().unwrap();
        channel.emit(event(1));
        let epoch = channel.lease.hub.inner.lock().unwrap().windows[&channel.lease.id]
            .epoch
            .clone();
        channel.emit(RunEvent::Line {
            line: "x".repeat(1024),
            when: barca_core::lines::LineWhen::Always,
        });
        assert_eq!(channel.bytes(), 0);
        channel.emit(event(3));
        finish(&channel);
        assert!(matches!(channel.resume(None), Err(CursorError::Expired)));
        let got = collect(&channel, Some(&format!("{epoch}:2"))).await;
        assert_eq!(
            got.iter().map(|frame| frame.sequence).collect::<Vec<_>>(),
            [3, 4]
        );
    }
    #[tokio::test]
    async fn reader_drains_retained_events_when_its_channel_drops_then_releases_the_window() {
        let hub = ReplayHub::default();
        let channel = hub.channel().unwrap();
        channel.emit(event(1));
        let stream = channel.resume(None).unwrap().unwrap();
        drop(channel);
        assert_eq!(stream.collect::<Vec<_>>().await.len(), 1);
        assert!(hub.inner.lock().unwrap().windows.is_empty());
    }
    #[tokio::test]
    async fn emission_between_attach_and_first_poll_is_not_lost() {
        let channel = RunChannel::new();
        let stream = channel.resume(None).unwrap().unwrap();
        channel.emit(event(1));
        finish(&channel);
        assert_eq!(stream.collect::<Vec<_>>().await.len(), 2);
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    fn state(root: &std::path::Path) -> crate::state::AppState {
        crate::state::AppState::new(crate::ServeConfig {
            files: Vec::new(),
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
            watch: false,
            schedule: false,
            timezone: "utc".into(),
            python: "python".into(),
            resolved: barca_core::config::resolve_in(None, root).unwrap(),
            read_only: true,
        })
    }
    async fn request(router: &axum::Router, cursor: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri("/runs/run/events");
        if let Some(cursor) = cursor {
            request = request.header("last-event-id", cursor);
        }
        router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn http_resume_statuses_and_ids_match_the_retained_window() {
        let root = tempfile::tempdir().unwrap();
        let state = state(root.path());
        let channel = state.replay.channel().unwrap();
        channel.emit(RunEvent::RunStarted {
            run_id: "run".into(),
        });
        channel.emit(RunEvent::RunFinished {
            run_id: "run".into(),
            ok: true,
            outcome: None,
        });
        state.events.insert("run".into(), channel.clone());
        let router = crate::routes::router(state);
        let response = request(&router, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = String::from_utf8(to_bytes(response.into_body(), 4096).await.unwrap().to_vec())
            .unwrap();
        let ids: Vec<_> = body
            .lines()
            .filter_map(|line| line.strip_prefix("id: "))
            .collect();
        assert_eq!(ids.len(), 2);
        let response = request(&router, Some(ids[0])).await;
        let resumed =
            String::from_utf8(to_bytes(response.into_body(), 4096).await.unwrap().to_vec())
                .unwrap();
        assert!(!resumed.contains("run_started"));
        assert!(resumed.contains("run_finished"));
        assert_eq!(
            request(&router, Some(ids[1])).await.status(),
            StatusCode::NO_CONTENT
        );
        for (cursor, status, reason) in [
            ("bad", StatusCode::BAD_REQUEST, "invalid_event_cursor"),
            (
                "00000000000000000000000000000000:1",
                StatusCode::CONFLICT,
                "event_cursor_mismatch",
            ),
        ] {
            let response = request(&router, Some(cursor)).await;
            assert_eq!(response.status(), status);
            let json: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(json["reason"], reason);
        }
    }
    #[tokio::test]
    async fn auth_is_checked_before_cursor_or_run_disclosure() {
        let root = tempfile::tempdir().unwrap();
        let mut state = state(root.path());
        let mut config = (*state.config).clone();
        config.resolved.server_auth = barca_core::config::AuthMode::Token;
        state.auth = Arc::new(crate::auth_http::Access::new(&config.resolved));
        state.config = Arc::new(config);
        let router = crate::routes::router(state);
        assert_eq!(
            request(&router, Some("bad")).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[test]
    fn replay_eviction_does_not_forget_live_step_progress() {
        let channel = ReplayHub::with_limits(Limits {
            events: 2,
            run_bytes: 4096,
            total_bytes: 4096,
        })
        .channel()
        .unwrap();
        channel.emit(RunEvent::StepFinished {
            node_id: "p.py:finished".into(),
            ok: true,
            cached: false,
            elapsed_seconds: None,
            error: None,
            import_seconds: None,
        });
        for n in 0..10 {
            channel.emit(RunEvent::log(
                "p.py:active".into(),
                n.to_string(),
                barca_core::LogStream::Stdout,
            ));
        }
        assert!(
            !channel
                .snapshot()
                .iter()
                .any(|event| matches!(event, RunEvent::StepFinished { .. }))
        );
        assert!(channel.progress().finished.contains("p.py:finished"));
        assert!(channel.progress().touched.contains("p.py:active"));
        channel.emit(RunEvent::RunFinished {
            run_id: "run".into(),
            ok: true,
            outcome: None,
        });
        assert!(channel.progress().finished.is_empty());
    }
}
