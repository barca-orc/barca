//! Following a run's event stream through a broken connection or a server restart
//! ([`Follow`]), and the state of one attempt to reopen it.

use super::sse::SseParser;
use super::{
    RECONNECT_ENV, RECONNECT_PAUSE, STREAM_IDLE, STREAM_LIMIT, STREAM_PROVEN, Server, encode,
    transport_reason,
};
use crate::error::{CliError, ErrorKind};
use barca_core::RunEvent;
use barca_core::outcome::Outcome;
use serde_json::Value;
use std::time::Duration;

#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;

/// The stream broke before the run ended: the run goes on on the server.
pub(super) fn dropped(server: &Server, run_id: &str, why: &str) -> CliError {
    CliError::from_prose(
        ErrorKind::Infra,
        format!(
            "error: lost the event stream of run {run_id} on the barca server at {}: {why}\n\
             The run continues on the server. See how it ended with `barca history --server {}` \
             (run {run_id}), or GET {}/runs/{run_id}.",
            server.shown_url, server.shown_url, server.shown_url
        ),
    )
}

/// The outcome in a `GET /runs/{run_id}` body.
pub(super) fn outcome_of(server: &Server, path: &str, body: &Value) -> Result<Outcome, CliError> {
    match &body["outcome"] {
        Value::Null => {
            Err(server.unexpected(&format!("GET {path}"), "the run ended without an outcome"))
        }
        outcome => Outcome::from_text(outcome.to_string())
            .map_err(|e| server.unexpected(&format!("GET {path}"), &e)),
    }
}

/// What following a run gives next.
pub(super) enum Followed {
    Event(RunEvent),
    /// The run ended while the stream was down: its `GET /runs/{run_id}` body.
    Ended(Value),
}

/// A run's event stream that rides through a server restart. When the stream breaks (the
/// server closed it without `run_finished`, the connection dropped, nothing came for
/// [`STREAM_IDLE`]) it reconnects for up to the window ([`reconnect_window`]): every
/// [`RECONNECT_PAUSE`] it asks `GET /health` and opens the stream again. The same server
/// (`started_at` unchanged) replays the run from its first event, and the events already read
/// are skipped; a restarted server streams the run as it runs there now (a queued run from its
/// start, a run that was stopped and requeued from the start of its new attempt). A run that
/// ended meanwhile is read from `GET /runs/{run_id}`. A server that does not come back within
/// the window ends the command as before: exit 3, naming the run.
pub(super) struct Follow<'a> {
    pub(super) server: &'a Server,
    pub(super) run_id: String,
    path: String,
    window: Duration,
    response: Option<reqwest::Response>,
    events: SseParser,
    ready: std::collections::VecDeque<(RunEvent, Option<String>)>,
    resume: bool,
    last_cursor: Option<String>,
    pending_cursor: Option<String>,
    polling: bool,
    poll_next: Option<tokio::time::Instant>,
    pub(super) complete_logs: bool,
    /// Events read from the current server.
    seen: usize,
    /// Events still to skip after reconnecting to the same server.
    skip: usize,
    /// The server's `started_at` (`None`: unknown, an older server).
    instance: Option<String>,
    /// Why the stream broke and until when to try again, while it is down.
    lost: Option<(String, tokio::time::Instant)>,
    /// The deadline of the last break and when the stream was opened again: a stream that
    /// breaks again within [`STREAM_PROVEN`] of a reconnect keeps that deadline, so a server
    /// that keeps cutting the stream does not get a fresh window each time.
    unproven: Option<(tokio::time::Instant, tokio::time::Instant)>,
    /// How many times the stream was opened again on a restarted server.
    pub(super) restarts: usize,
}

impl<'a> Follow<'a> {
    pub(super) async fn new(server: &'a Server, run_id: &str, window: Duration) -> Self {
        let health = server.get_json("/health").await.ok();
        let resume = health.as_ref().is_some_and(resume_capability);
        let instance = health.and_then(started_at);
        Self {
            server,
            run_id: run_id.to_string(),
            path: format!("/runs/{}/events", encode(run_id)),
            window,
            response: None,
            events: SseParser::default(),
            ready: Default::default(),
            seen: 0,
            skip: 0,
            instance,
            resume,
            last_cursor: None,
            pending_cursor: None,
            polling: false,
            poll_next: None,
            complete_logs: false,
            lost: None,
            unproven: None,
            restarts: 0,
        }
    }

    /// Open the stream: `true` when the run is live. `false` (404) when it is not live: it
    /// ended, or this server never ran it. A server that cannot be reached is ridden through.
    pub(super) async fn open(&mut self) -> Result<bool, CliError> {
        match self.request().await {
            Ok(Some(response)) => {
                self.response = Some(response);
                Ok(true)
            }
            Ok(None) => match self
                .json_attempt(&format!("/runs/{}", encode(&self.run_id)))
                .await
            {
                Ok(body) if matches!(body["status"].as_str(), Some("pending" | "running")) => {
                    self.lose("the run has no live stream yet".into())?;
                    Ok(true)
                }
                Ok(_) => Ok(false),
                Err(Attempt::Unreachable(why)) => {
                    self.lose(why)?;
                    Ok(true)
                }
                Err(Attempt::Refused(error)) => Err(error),
                Err(_) => unreachable!("JSON requests have no cursor"),
            },
            Err(Attempt::Refused(e)) => Err(e),
            Err(Attempt::Expired) => {
                self.gap()?;
                Ok(true)
            }
            Err(Attempt::Mismatch) => Err(self.server.unexpected(
                "event stream",
                "unexpected attempt mismatch before attachment",
            )),
            Err(Attempt::Unreachable(why)) => {
                self.lose(why)?;
                Ok(true)
            }
        }
    }

    /// `GET /runs/{run_id}/events`: the stream, `None` for a 404, or why not.
    async fn request(&self) -> Result<Option<reqwest::Response>, Attempt> {
        let mut request = self
            .server
            .http
            .request(reqwest::Method::GET, &self.path)
            .map_err(Attempt::Refused)?
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .timeout(STREAM_LIMIT);
        if self.resume
            && let Some(cursor) = &self.last_cursor
        {
            request = request.header("last-event-id", cursor);
        }
        let response = request.send().await.map_err(|error| {
            Attempt::Unreachable(self.server.http.redact(&transport_reason(&error)))
        })?;
        let status = response.status();
        if matches!(
            status,
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(None);
        }
        if self.resume && status == reqwest::StatusCode::GONE {
            return Err(Attempt::Expired);
        }
        if self.resume && status == reqwest::StatusCode::CONFLICT {
            return Err(Attempt::Mismatch);
        }
        if status.is_success() {
            return Ok(Some(response));
        }
        // A proxy in front of a server that is restarting answers 502/503/504.
        if status.is_server_error() {
            return Err(Attempt::Unreachable(format!("HTTP {status}")));
        }
        let body = response.bytes().await.unwrap_or_default();
        Err(Attempt::Refused(self.server.refusal(
            status,
            &body,
            &format!("GET {}", self.path),
        )))
    }

    /// A request whose status remains available to the reconnect policy.
    async fn json_attempt(&self, path: &str) -> Result<Value, Attempt> {
        let response = self
            .server
            .http
            .request(reqwest::Method::GET, path)
            .map_err(Attempt::Refused)?
            .send()
            .await
            .map_err(|error| {
                Attempt::Unreachable(self.server.http.redact(&transport_reason(&error)))
            })?;
        let status = response.status();
        if status.is_server_error() {
            return Err(Attempt::Unreachable(format!("HTTP {status}")));
        }
        let body = response.bytes().await.map_err(|error| {
            Attempt::Unreachable(self.server.http.redact(&transport_reason(&error)))
        })?;
        if !status.is_success() {
            return Err(Attempt::Refused(self.server.refusal(
                status,
                &body,
                &format!("GET {path}"),
            )));
        }
        serde_json::from_slice(&body).map_err(|_| {
            Attempt::Refused(
                self.server
                    .unexpected(&format!("GET {path}"), "invalid JSON"),
            )
        })
    }

    fn gap(&mut self) -> Result<(), CliError> {
        if self.complete_logs {
            return Err(CliError::from_prose(
                ErrorKind::Infra,
                format!(
                    "event replay has an unfillable log gap for run {}\nRead its recorded logs with `barca logs {} --server {}`; this live transcript is incomplete.",
                    self.run_id, self.run_id, self.server.shown_url
                ),
            ));
        }
        barca_core::errln!(
            "[barca] event replay is incomplete for run {}; following its outcome by polling; recorded logs remain available",
            self.run_id
        );
        self.response = None;
        self.ready.clear();
        self.events = SseParser::default();
        self.lost = None;
        self.unproven = None;
        self.polling = true;
        self.poll_next = None;
        Ok(())
    }

    async fn poll_outcome(&mut self) -> Result<Followed, CliError> {
        let path = format!("/runs/{}", encode(&self.run_id));
        loop {
            if let Some(next) = self.poll_next {
                tokio::time::sleep_until(next).await;
            }
            self.poll_next = Some(tokio::time::Instant::now() + Duration::from_secs(2));
            match self.json_attempt(&path).await {
                Ok(body) => {
                    // A healthy pending/running answer resets only transport-outage state;
                    // the run itself can remain healthy longer than the reconnect window.
                    self.lost = None;
                    if !matches!(body["status"].as_str(), Some("pending" | "running")) {
                        return Ok(Followed::Ended(body));
                    }
                }
                Err(Attempt::Refused(error)) => return Err(error),
                Err(Attempt::Unreachable(why)) => {
                    if self.window.is_zero() {
                        return Err(dropped(self.server, &self.run_id, &why));
                    }
                    let (_, deadline) = self.lost.get_or_insert_with(|| {
                        (why.clone(), tokio::time::Instant::now() + self.window)
                    });
                    if tokio::time::Instant::now() >= *deadline {
                        return Err(dropped(self.server, &self.run_id, &why));
                    }
                    self.poll_next = Some(tokio::time::Instant::now() + RECONNECT_PAUSE);
                }
                Err(Attempt::Expired | Attempt::Mismatch) => {
                    unreachable!("JSON requests have no cursor")
                }
            }
        }
    }

    /// The stream broke for `why`: start waiting for the server, or give up when the window
    /// is zero.
    fn lose(&mut self, why: String) -> Result<(), CliError> {
        self.response = None;
        if self.window.is_zero() {
            return Err(dropped(self.server, &self.run_id, &why));
        }
        if let Some((deadline, opened)) = self.unproven.take()
            && opened.elapsed() < STREAM_PROVEN
        {
            self.lost = Some((why, deadline));
            return Ok(());
        }
        barca_core::errln!(
            "[barca] lost the event stream of run {} ({why}); waiting up to {}s for the server \
             to come back",
            self.run_id,
            self.window.as_secs()
        );
        self.lost = Some((why, tokio::time::Instant::now() + self.window));
        Ok(())
    }

    /// The next event, riding through a broken stream.
    pub(super) async fn next(&mut self) -> Result<Followed, CliError> {
        // Callers request another event only after successfully handling the previous one.
        if let Some(cursor) = self.pending_cursor.take() {
            self.last_cursor = Some(cursor);
        }
        loop {
            if self.polling {
                return self.poll_outcome().await;
            }
            if let Some((event, cursor)) = self.ready.pop_front() {
                if self.resume {
                    let next = cursor.as_deref().and_then(cursor_parts);
                    let previous = self.last_cursor.as_deref().and_then(cursor_parts);
                    if let (Some((epoch, sequence)), Some((prior_epoch, prior_sequence))) =
                        (next, previous)
                        && epoch == prior_epoch
                        && sequence <= prior_sequence
                    {
                        continue;
                    }
                    let contiguous = match (previous, next) {
                        (None, Some((_, 1))) => true,
                        (Some((epoch, sequence)), Some((next_epoch, next_sequence))) => {
                            epoch == next_epoch && sequence.checked_add(1) == Some(next_sequence)
                        }
                        _ => false,
                    };
                    if !contiguous {
                        self.gap()?;
                        continue;
                    }
                }
                self.pending_cursor = cursor;
                return Ok(Followed::Event(event));
            }
            if self.lost.is_some() {
                if let Some(ended) = self.reconnect().await? {
                    return Ok(ended);
                }
                continue;
            }
            let Some(response) = self.response.as_mut() else {
                self.lose("the stream was not open".into())?;
                continue;
            };
            let bytes = match tokio::time::timeout(STREAM_IDLE, response.chunk()).await {
                Err(_) => {
                    self.lose(format!("nothing received for {}s", STREAM_IDLE.as_secs()))?;
                    continue;
                }
                Ok(Err(e)) => {
                    self.lose(self.server.http.redact(&transport_reason(&e)))?;
                    continue;
                }
                Ok(Ok(None)) => {
                    self.lose("the server closed the stream".into())?;
                    continue;
                }
                Ok(Ok(Some(bytes))) => bytes,
            };
            for frame in self
                .events
                .push(&bytes)
                .map_err(|why| self.server.unexpected(&format!("GET {}", self.path), &why))?
            {
                if self.resume && !frame.id.as_deref().is_some_and(valid_cursor) {
                    return Err(self
                        .server
                        .unexpected("event stream", "missing or invalid attempt cursor"));
                }
                let event = RunEvent::parse(&frame.data).map_err(|e| {
                    self.server
                        .unexpected(&format!("GET {}", self.path), &e.to_string())
                })?;
                self.seen += 1;
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                self.ready.push_back((event, frame.id));
            }
        }
    }

    /// Try, until the deadline, to follow the run again. `Some` when it ended meanwhile.
    async fn reconnect(&mut self) -> Result<Option<Followed>, CliError> {
        let Some((why, mut deadline)) = self.lost.clone() else {
            return Ok(None);
        };
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(dropped(
                    self.server,
                    &self.run_id,
                    &format!(
                        "{why}; the server did not come back within {}s ({RECONNECT_ENV})",
                        self.window.as_secs()
                    ),
                ));
            }
            tokio::time::sleep(RECONNECT_PAUSE).await;
            let health = match self.json_attempt("/health").await {
                Ok(health) => health,
                Err(Attempt::Refused(error)) => return Err(error),
                Err(_) => continue,
            };
            self.resume = resume_capability(&health);
            if self.resume && !self.last_cursor.as_deref().is_some_and(valid_cursor) {
                self.last_cursor = None;
            }
            let instance = started_at(health);
            match self.request().await {
                Err(Attempt::Unreachable(_)) => continue,
                Err(Attempt::Refused(e)) => return Err(e),
                Err(Attempt::Expired) => {
                    self.gap()?;
                    return Ok(None);
                }
                Err(Attempt::Mismatch) => {
                    if self.last_cursor.is_none() {
                        return Err(self.server.unexpected(
                            "event stream",
                            "attempt mismatch without a resume cursor",
                        ));
                    }
                    let body = self.recorded().await?;
                    let ended = !matches!(body["status"].as_str(), Some("pending" | "running"));
                    self.last_cursor = None;
                    self.pending_cursor = None;
                    self.seen = 0;
                    self.skip = 0;
                    self.events = SseParser::default();
                    self.restarts += 1;
                    self.instance = instance;
                    deadline = tokio::time::Instant::now() + self.window;
                    self.lost = Some((why.clone(), deadline));
                    barca_core::errln!(
                        "[barca] run {} has a new attempt; following its new stream without submitting another run",
                        self.run_id
                    );
                    if ended {
                        self.lost = None;
                        return Ok(Some(Followed::Ended(body)));
                    }
                    continue;
                }
                Ok(None) => {
                    let body = self.recorded().await?;
                    if matches!(body["status"].as_str(), Some("pending" | "running")) {
                        // Live in the history but not streamed yet: ask again.
                        continue;
                    }
                    self.lost = None;
                    if !(instance.is_some() && instance == self.instance) {
                        self.restarts += 1;
                    }
                    barca_core::errln!(
                        "[barca] the server is back; run {} ended meanwhile ({})",
                        self.run_id,
                        body["status"].as_str().unwrap_or("unknown")
                    );
                    return Ok(Some(Followed::Ended(body)));
                }
                Ok(Some(response)) => {
                    let same = instance.is_some() && instance == self.instance;
                    self.skip = if same && !self.resume { self.seen } else { 0 };
                    self.seen = 0;
                    self.events = SseParser::default();
                    self.response = Some(response);
                    self.lost = None;
                    self.unproven = Some((deadline, tokio::time::Instant::now()));
                    if same {
                        barca_core::errln!("[barca] reconnected; following run {}", self.run_id);
                    } else {
                        self.restarts += 1;
                        barca_core::errln!(
                            "[barca] the server restarted; following run {} there (a run \
                             that was stopped is shown again from its new start)",
                            self.run_id
                        );
                    }
                    self.instance = instance;
                    return Ok(None);
                }
            }
        }
    }

    /// `GET /runs/{run_id}`.
    async fn recorded(&self) -> Result<Value, CliError> {
        self.server
            .get_json(&format!("/runs/{}", encode(&self.run_id)))
            .await
    }

    /// The outcome of a run that is not live, from `GET /runs/{run_id}`.
    pub(super) async fn recorded_outcome(&self) -> Result<Outcome, CliError> {
        let body = self.recorded().await?;
        outcome_of(self.server, &format!("/runs/{}", self.run_id), &body)
    }
}

/// Why an attempt to open a run's stream failed.
enum Attempt {
    Expired,
    Mismatch,
    /// No answer, or one from a proxy whose server is away: try again.
    Unreachable(String),
    /// The server's answer: stop.
    Refused(CliError),
}

fn resume_capability(health: &Value) -> bool {
    health["capabilities"]["event_resume"].as_u64() == Some(1)
}
fn valid_cursor(cursor: &str) -> bool {
    let Some((epoch, sequence)) = cursor.split_once(':') else {
        return false;
    };
    cursor.len() <= 128
        && epoch.len() == 32
        && epoch
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !sequence.starts_with('0')
        && sequence.bytes().all(|b| b.is_ascii_digit())
        && sequence.parse::<u64>().is_ok_and(|sequence| sequence > 0)
}

fn cursor_parts(cursor: &str) -> Option<(&str, u64)> {
    if !valid_cursor(cursor) {
        return None;
    }
    let (epoch, sequence) = cursor.split_once(':')?;
    Some((epoch, sequence.parse().ok()?))
}

/// `started_at` of a `GET /health` body.
fn started_at(health: Value) -> Option<String> {
    health["started_at"].as_str().map(str::to_string)
}
