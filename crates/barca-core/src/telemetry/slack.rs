//! Slack: a message to an incoming webhook when a job starts failing, when it recovers, and
//! once a day while it keeps failing.
//!
//! `BARCA_SLACK_WEBHOOK_URL` alone switches it on (see [`super::configured`]). Settings:
//!
//! - `BARCA_SLACK_NOTIFY`: `changes` (default), `failures` or `all`;
//! - `BARCA_UI_URL`: base URL of the web UI, for a link to the run.
//!
//! Whether a run is a change is decided against the job's earlier finished runs in the
//! metadata database and the last post for it ([`JobHistory`]), so it works the same across
//! separate CLI processes and across a server restart. A post that is not delivered is not
//! recorded, so the next run retries it.
//!
//! The webhook URL is a secret: it is never printed. A delivery error names its category only.

use super::history::{JobHistory, JobRecord, PostState};
use super::{EXPORT_TIMEOUT, Integration, RunReport, StepOutcome, StepReport};
use chrono::NaiveDateTime;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// The name in the telemetry layer, in warnings and in the `notifications` table.
pub const NAME: &str = "slack";
/// The switch: an incoming webhook URL. Only this name turns the integration on; a generic
/// `SLACK_WEBHOOK_URL` that other tools set is ignored.
pub const WEBHOOK_VAR: &str = "BARCA_SLACK_WEBHOOK_URL";
/// While a job keeps failing, the next reminder is due this long after the last post.
const REMIND_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// The first line of an exception message is cut to this many characters.
const MAX_ERROR_CHARS: usize = 300;
/// A job naming many targets is cut to this many characters in the message.
const MAX_JOB_CHARS: usize = 200;

/// What `BARCA_SLACK_NOTIFY` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Starts failing, recovers, and the daily reminder.
    Changes,
    /// Every failed run, and recovers.
    Failures,
    /// Every finished run that executed at least one step, and recovers.
    All,
}

impl Mode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "changes" => Some(Self::Changes),
            "failures" => Some(Self::Failures),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

pub struct Slack {
    webhook: String,
    mode: Mode,
    ui_url: Option<String>,
    remind_after: Duration,
    client: OnceLock<Result<reqwest::Client, String>>,
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Whether `url` is something a webhook can be: `https://` and a host. With `allow_http`
/// (a test hook), plain `http://` too.
fn webhook_ok(url: &str, allow_http: bool) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = match (
        lower.strip_prefix("https://"),
        lower.strip_prefix("http://"),
    ) {
        (Some(rest), _) => rest,
        (None, Some(rest)) if allow_http => rest,
        _ => return false,
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    !host.is_empty() && !host.contains(char::is_whitespace)
}

impl Slack {
    /// `Ok(None)` while `BARCA_SLACK_WEBHOOK_URL` is unset or empty. An `Err` never contains
    /// the URL.
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(webhook) = env_var(WEBHOOK_VAR) else {
            return Ok(None);
        };
        // Test hooks, not part of the contract: a fake webhook speaks plain HTTP, and the
        // reminder is due sooner.
        let allow_http = env_var("BARCA_TEST_SLACK_ALLOW_HTTP").is_some();
        let remind_after = env_var("BARCA_TEST_SLACK_REMIND_SECS")
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(REMIND_AFTER, Duration::from_secs);
        if !webhook_ok(&webhook, allow_http) {
            return Err(format!("{WEBHOOK_VAR} is not an https:// URL"));
        }
        let mode = match env_var("BARCA_SLACK_NOTIFY") {
            None => Mode::Changes,
            Some(raw) => Mode::parse(&raw).unwrap_or_else(|| {
                crate::warnings::warn_once(
                    "BARCA_SLACK_NOTIFY must be changes, failures or all; using changes",
                );
                Mode::Changes
            }),
        };
        Ok(Some(Self {
            webhook,
            mode,
            ui_url: env_var("BARCA_UI_URL").map(|u| u.trim_end_matches('/').to_string()),
            remind_after,
            client: OnceLock::new(),
        }))
    }

    /// Built on the first post, not at startup: most runs post nothing.
    fn client(&self) -> Result<&reqwest::Client, String> {
        self.client
            .get_or_init(|| crate::http::client(EXPORT_TIMEOUT))
            .as_ref()
            .map_err(Clone::clone)
    }

    async fn post(&self, body: &Value) -> Result<(), String> {
        let body = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        let reply = self
            .client()?
            .post(&self.webhook)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| delivery_error(&e))?;
        let status = reply.status();
        if status.is_success() {
            return Ok(());
        }
        Err(match status.as_u16() {
            429 => "HTTP 429 (rate limited)".to_string(),
            code => format!("HTTP {code}"),
        })
    }
}

/// The category of a failed request. Never the error's own text: reqwest names the URL.
fn delivery_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        format!("no reply within {}s", EXPORT_TIMEOUT.as_secs())
    } else if e.is_connect() {
        "connection failed".to_string()
    } else {
        "request failed".to_string()
    }
}

/// What to post about a finished run, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Message {
    Failing,
    StillFailing,
    Recovered,
    Succeeded,
}

impl Message {
    fn state(self) -> PostState {
        match self {
            Self::Failing => PostState::Failing,
            Self::StillFailing => PostState::StillFailing,
            Self::Recovered => PostState::Recovered,
            Self::Succeeded => PostState::Succeeded,
        }
    }
}

/// The decision, from the run's status and what the database knows about the job.
///
/// A failing post is *open* while no success of the job was recorded after it. A failed run
/// with an open post is a repeat (in `changes` mode, a reminder once `remind_after` has
/// passed); without one it is a new failure, which also covers a failure whose post was
/// never delivered. A success after a failed run, or with a failing post never answered by a
/// recovery, is a recovery.
pub(crate) fn decide(
    mode: Mode,
    status: &str,
    steps_executed: usize,
    job: &JobRecord,
    now: NaiveDateTime,
    remind_after: Duration,
) -> Option<Message> {
    let failing_post = job.last_post.as_ref().filter(|p| p.state.is_failing());
    let open = failing_post.filter(|p| job.last_success_at.is_none_or(|s| p.posted_at >= s));
    match status {
        "failed" => match (mode, open) {
            (Mode::Changes, Some(post)) => {
                let since = now.signed_duration_since(post.posted_at);
                (since.to_std().unwrap_or_default() >= remind_after)
                    .then_some(Message::StillFailing)
            }
            (Mode::Changes, None) => Some(Message::Failing),
            (Mode::Failures | Mode::All, open) => {
                if open.is_some() || job.previous.as_deref() == Some("failed") {
                    Some(Message::StillFailing)
                } else {
                    Some(Message::Failing)
                }
            }
        },
        "success" => {
            if job.previous.as_deref() == Some("failed") || failing_post.is_some() {
                Some(Message::Recovered)
            } else if mode == Mode::All && steps_executed > 0 {
                Some(Message::Succeeded)
            } else {
                None
            }
        }
        // `cancelled` and skipped ticks are not outcomes of the job.
        _ => None,
    }
}

/// The first `max` characters of `text`, with `…` when something was dropped.
fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Slack's mrkdwn treats these three as control characters.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `350ms`, `4.2s`, `4m 12s`, `2h 3m`.
fn duration(ns: u64) -> String {
    let ms = ns / 1_000_000;
    let secs = ns / 1_000_000_000;
    match secs {
        0 => format!("{ms}ms"),
        1..60 => format!("{:.1}s", ns as f64 / 1e9),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// `TypeError: first line of the message`, cut; the traceback is never sent.
fn error_line(step: &StepReport) -> Option<String> {
    let kind = step.error_type.as_deref().filter(|t| !t.is_empty());
    let first = step
        .error_message
        .as_deref()
        .and_then(|m| m.lines().map(str::trim).find(|l| !l.is_empty()))
        .map(|l| cut(l, MAX_ERROR_CHARS));
    match (kind, first) {
        (Some(kind), Some(first)) if first.starts_with(kind) => Some(first),
        (Some(kind), Some(first)) => Some(format!("{kind}: {first}")),
        (Some(kind), None) => Some(kind.to_string()),
        (None, first) => first,
    }
}

/// `2026-10-19 06:00 UTC` from SQLite's `datetime('now')` text.
fn since(stamp: &str) -> String {
    format!("{} UTC", stamp.get(..16).unwrap_or(stamp))
}

fn plural(n: u64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// The message as lines of plain text; the first is the headline. Block Kit and the `text`
/// fallback are both built from these.
struct Lines {
    headline: String,
    body: Vec<String>,
    footer: Vec<String>,
}

impl Slack {
    fn lines(&self, message: Message, run: &RunReport, job: &JobRecord) -> Lines {
        let name = cut(&run.job, MAX_JOB_CHARS);
        let failed: Vec<&StepReport> = run
            .steps
            .iter()
            .filter(|s| s.outcome == StepOutcome::Failed)
            .collect();
        let streak = || {
            let runs = plural(job.failed_runs.max(1), "run");
            match &job.failing_since {
                Some(stamp) => format!("Failing since {} ({runs})", since(stamp)),
                None => format!("Failing ({runs})"),
            }
        };
        let mut body = Vec::new();
        let headline = match message {
            Message::Failing => format!("🔴 {name} is failing"),
            Message::StillFailing => format!("🔴 {name} is still failing"),
            Message::Recovered => format!("✅ {name} recovered"),
            Message::Succeeded => format!("✅ {name} succeeded"),
        };
        match message {
            Message::Failing => {
                if let Some(step) = failed.first() {
                    let mut line = format!("Step {} failed", step.node_id);
                    if let Some(n) = step.attempts {
                        line.push_str(&format!(" after {}", plural(u64::from(n), "attempt")));
                    }
                    line.push_str(&format!(" in {}", duration(step.duration_ns)));
                    if failed.len() > 1 {
                        line.push_str(&format!(" ({} steps failed)", failed.len()));
                    }
                    body.push(line);
                    body.extend(error_line(step));
                }
            }
            Message::StillFailing => {
                let mut line = streak();
                if let Some(error) = failed.first().and_then(|s| error_line(s)) {
                    line.push_str(&format!(", last error {error}"));
                }
                body.push(line);
            }
            Message::Recovered => body.push(streak()),
            Message::Succeeded => body.push(format!(
                "{} ran, {} cached in {}",
                plural(run.steps_executed as u64, "step"),
                run.steps_cached,
                duration(run.duration_ns)
            )),
        }
        let mut context = Vec::new();
        if let Some(env) = &run.env {
            context.push(format!("env {env}"));
        }
        let host = crate::db::local_host();
        if !host.is_empty() {
            context.push(format!("host {host}"));
        }
        context.push(format!("run {}", run.run_id));
        let mut footer = vec![context.join(" · ")];
        if matches!(message, Message::Failing | Message::StillFailing) {
            footer.push(format!("barca history {}", run.run_id));
        }
        Lines {
            headline,
            body,
            footer,
        }
    }

    /// One Block Kit message with a plain `text` fallback (notifications and clients that do
    /// not render blocks show the fallback).
    pub(crate) fn payload(&self, message: Message, run: &RunReport, job: &JobRecord) -> Value {
        let lines = self.lines(message, run, job);
        let text = std::iter::once(lines.headline.clone())
            .chain(lines.body.iter().cloned())
            .chain(lines.footer.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        let mut blocks = vec![json!({
            "type": "section",
            "text": {"type": "mrkdwn", "text": format!("*{}*", escape(&lines.headline))},
        })];
        if !lines.body.is_empty() {
            let body: Vec<String> = lines.body.iter().map(|l| escape(l)).collect();
            blocks.push(json!({
                "type": "section",
                "text": {"type": "mrkdwn", "text": body.join("\n")},
            }));
        }
        let mut context: Vec<Value> = Vec::new();
        for (i, line) in lines.footer.iter().enumerate() {
            let mut text = escape(line);
            if i == 0
                && let Some(ui) = &self.ui_url
            {
                let run = escape(&run.run_id);
                text = text.replace(
                    &format!("run {run}"),
                    &format!("run <{}/#/runs?run={run}|{run}>", escape(ui)),
                );
            }
            if i > 0 {
                text = format!("`{text}`");
            }
            context.push(json!({"type": "mrkdwn", "text": text}));
        }
        blocks.push(json!({"type": "context", "elements": context}));
        json!({"text": text, "blocks": blocks})
    }
}

impl Integration for Slack {
    fn export<'a>(&'a self, run: &'a RunReport, history: &'a JobHistory) -> super::Export<'a> {
        Box::pin(async move {
            // Only a finished outcome changes a job's state. A skipped tick and a cancelled
            // run never post and never read the database.
            if !matches!(run.status.as_str(), "success" | "failed") {
                return Ok(());
            }
            // A run with nothing recorded (no metadata database) has no history to compare.
            let Some(job) = history.read(&run.job, &run.run_id, NAME).await? else {
                return Ok(());
            };
            let now = chrono::Utc::now().naive_utc();
            let Some(message) = decide(
                self.mode,
                &run.status,
                run.steps_executed,
                &job,
                now,
                self.remind_after,
            ) else {
                return Ok(());
            };
            self.post(&self.payload(message, run, &job)).await?;
            history
                .record_post(&run.job, NAME, message.state(), now)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::history::Post;
    use super::*;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn job(previous: Option<&str>, last_post: Option<(&str, PostState)>) -> JobRecord {
        JobRecord {
            previous: previous.map(str::to_string),
            last_success_at: None,
            failing_since: Some("2026-10-19 06:00:00".into()),
            failed_runs: 4,
            last_post: last_post.map(|(t, state)| Post {
                posted_at: at(t),
                state,
            }),
        }
    }

    const DAY: Duration = REMIND_AFTER;

    #[test]
    fn changes_posts_the_transitions_and_a_daily_reminder() {
        let now = at("2026-10-20 06:00:00");
        let d = |status, job: &JobRecord| decide(Mode::Changes, status, 1, job, now, DAY);
        // First failure, with or without an earlier success.
        assert_eq!(d("failed", &job(None, None)), Some(Message::Failing));
        assert_eq!(
            d("failed", &job(Some("success"), None)),
            Some(Message::Failing)
        );
        // Repeat failure: nothing until a day after the last post, then a reminder.
        let posted = job(
            Some("failed"),
            Some(("2026-10-19 07:00:00", PostState::Failing)),
        );
        assert_eq!(d("failed", &posted), None);
        let day_old = job(
            Some("failed"),
            Some(("2026-10-19 06:00:00", PostState::StillFailing)),
        );
        assert_eq!(d("failed", &day_old), Some(Message::StillFailing));
        // A failure whose post was never delivered is retried as a new failure.
        assert_eq!(
            d("failed", &job(Some("failed"), None)),
            Some(Message::Failing)
        );
        // Recovery, and nothing for success after success.
        assert_eq!(d("success", &posted), Some(Message::Recovered));
        assert_eq!(
            d("success", &job(Some("failed"), None)),
            Some(Message::Recovered)
        );
        assert_eq!(d("success", &job(Some("success"), None)), None);
        let recovered = job(
            Some("success"),
            Some(("2026-10-19 07:00:00", PostState::Recovered)),
        );
        assert_eq!(d("success", &recovered), None);
        assert_eq!(d("failed", &recovered), Some(Message::Failing));
        // Not outcomes.
        assert_eq!(d("cancelled", &posted), None);
        assert_eq!(d("skipped", &job(None, None)), None);
    }

    #[test]
    fn a_failing_post_answered_by_a_later_success_is_closed() {
        let now = at("2026-10-19 08:00:00");
        let mut j = job(
            Some("success"),
            Some(("2026-10-19 06:00:00", PostState::Failing)),
        );
        j.last_success_at = Some(at("2026-10-19 07:00:00"));
        // The recovery was never delivered: a new failure is a new streak.
        assert_eq!(
            decide(Mode::Changes, "failed", 1, &j, now, DAY),
            Some(Message::Failing)
        );
    }

    #[test]
    fn failures_and_all_post_every_failure_and_all_posts_executed_successes() {
        let now = at("2026-10-20 06:00:00");
        let posted = job(
            Some("failed"),
            Some(("2026-10-20 05:59:00", PostState::Failing)),
        );
        for mode in [Mode::Failures, Mode::All] {
            assert_eq!(
                decide(mode, "failed", 1, &job(Some("success"), None), now, DAY),
                Some(Message::Failing)
            );
            assert_eq!(
                decide(mode, "failed", 1, &posted, now, DAY),
                Some(Message::StillFailing)
            );
            assert_eq!(
                decide(mode, "success", 1, &posted, now, DAY),
                Some(Message::Recovered)
            );
        }
        let ok = job(Some("success"), None);
        assert_eq!(decide(Mode::Failures, "success", 1, &ok, now, DAY), None);
        assert_eq!(
            decide(Mode::All, "success", 1, &ok, now, DAY),
            Some(Message::Succeeded)
        );
        // A fully cached run executed nothing: only a recovery is posted for it.
        assert_eq!(decide(Mode::All, "success", 0, &ok, now, DAY), None);
        assert_eq!(
            decide(Mode::All, "success", 0, &posted, now, DAY),
            Some(Message::Recovered)
        );
    }

    #[test]
    fn notify_values_parse_case_insensitively() {
        assert_eq!(Mode::parse(" Changes "), Some(Mode::Changes));
        assert_eq!(Mode::parse("FAILURES"), Some(Mode::Failures));
        assert_eq!(Mode::parse("all"), Some(Mode::All));
        assert_eq!(Mode::parse("every"), None);
    }

    #[test]
    fn only_https_webhooks_are_accepted() {
        assert!(webhook_ok("https://hooks.slack.com/services/T/B/X", false));
        assert!(webhook_ok("HTTPS://hooks.slack.com/x", false));
        assert!(!webhook_ok("http://hooks.slack.com/x", false));
        assert!(webhook_ok("http://127.0.0.1:9/x", true));
        assert!(!webhook_ok("https:///x", false));
        assert!(!webhook_ok("hooks.slack.com/x", false));
        assert!(!webhook_ok("ftp://x", true));
    }

    fn failed_run() -> RunReport {
        RunReport {
            run_id: "0179afe0cb58".into(),
            command: "get".into(),
            target: Some("daily_revenue".into()),
            job: "p.py:daily_revenue".into(),
            env: Some("prod".into()),
            status: "failed".into(),
            reason: None,
            in_flight_run: None,
            trigger: Default::default(),
            start_unix_ns: 0,
            duration_ns: 252_000_000_000,
            steps_total: 2,
            steps_executed: 1,
            steps_cached: 0,
            steps: vec![StepReport {
                node_id: "p.py:orders".into(),
                kind: "asset",
                outcome: StepOutcome::Failed,
                start_unix_ns: 0,
                duration_ns: 252_000_000_000,
                attempts: Some(3),
                run_hash: None,
                size_bytes: None,
                cpu_seconds: None,
                max_rss_bytes: None,
                error_type: Some("KeyError".into()),
                error_message: Some(format!("'region' <{}>\nsecond line", "x".repeat(400))),
                error_traceback: Some("Traceback (most recent call last):\n  secret".into()),
            }],
        }
    }

    fn slack(ui_url: Option<&str>) -> Slack {
        Slack {
            webhook: "https://hooks.slack.com/services/T/B/X".into(),
            mode: Mode::Changes,
            ui_url: ui_url.map(str::to_string),
            remind_after: REMIND_AFTER,
            client: OnceLock::new(),
        }
    }

    #[test]
    fn a_failing_message_names_the_step_error_and_run_but_not_the_traceback() {
        let run = failed_run();
        let body = slack(None).payload(Message::Failing, &run, &job(None, None));
        let text = body["text"].as_str().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "🔴 p.py:daily_revenue is failing");
        assert_eq!(
            lines[1],
            "Step p.py:orders failed after 3 attempts in 4m 12s"
        );
        assert!(
            lines[2].starts_with("KeyError: 'region' <xxx"),
            "{}",
            lines[2]
        );
        assert!(lines[2].ends_with('…'));
        assert_eq!(
            lines[2].chars().count(),
            "KeyError: ".len() + MAX_ERROR_CHARS + 1
        );
        assert!(lines[3].starts_with("env prod · "), "{}", lines[3]);
        assert!(lines[3].ends_with("run 0179afe0cb58"));
        assert_eq!(lines[4], "barca history 0179afe0cb58");
        assert!(!text.contains("Traceback") && !text.contains("second line"));
        // Block Kit: sections and a context block, mrkdwn escaped.
        let blocks = body["blocks"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "section");
        assert!(
            blocks[1]["text"]["text"]
                .as_str()
                .unwrap()
                .contains("&lt;xxx")
        );
        assert_eq!(blocks.last().unwrap()["type"], "context");
        assert!(!body.to_string().contains("Traceback"));
    }

    #[test]
    fn the_run_links_to_the_ui_when_its_url_is_set() {
        let run = failed_run();
        let body = slack(Some("https://barca.example.com/ui")).payload(
            Message::Recovered,
            &run,
            &job(Some("failed"), None),
        );
        let context = body["blocks"].as_array().unwrap().last().unwrap()["elements"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            context.contains(
                "run <https://barca.example.com/ui/#/runs?run=0179afe0cb58|0179afe0cb58>"
            ),
            "{context}"
        );
        let text = body["text"].as_str().unwrap();
        assert!(text.starts_with("✅ p.py:daily_revenue recovered\n"));
        assert!(text.contains("Failing since 2026-10-19 06:00 UTC (4 runs)"));
        assert!(!text.contains("barca history"));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(350_000_000), "350ms");
        assert_eq!(duration(4_200_000_000), "4.2s");
        assert_eq!(duration(252_000_000_000), "4m 12s");
        assert_eq!(duration(7_380_000_000_000), "2h 3m");
    }
}
