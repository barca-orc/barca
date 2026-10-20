---
title: 'RFC-0010: Slack Notifications'
description: 'Setting BARCA_SLACK_WEBHOOK_URL posts to a Slack incoming webhook when a job starts failing, when it recovers, and once a day while it keeps failing. No new config file, no hooks system.'
---

- **Status:** Accepted
- **Date:** 2026-10-20 (accepted 2026-10-20)
- **Touches:** barca-core | HTTP server
- **Supersedes / Related:** Reopens parked issue #52 (Resend email and Slack webhook hooks), with a smaller design. Builds on the telemetry integration layer (the `telemetry` manual topic). Related: #434 (trigger and skip reasons on run spans), #393 (`triggered_by`).

---

## 1. Summary

Setting `BARCA_SLACK_WEBHOOK_URL` turns on a Slack integration built into every install. It
posts to a Slack [incoming webhook](https://api.slack.com/messaging/webhooks) when a job **starts
failing**, when it **recovers**, and **once a day while it keeps failing**. It never posts on
an ordinary success, a cached run, a skipped tick or a cancelled run. It works for `barca get`,
`barca run` and every run `barca serve` starts, like the Datadog integration.

The webhook variable is the whole switch: there is nothing to install and nothing to name in
`BARCA_TELEMETRY`. Two optional variables tune it. There is no `barca.toml` section, no hooks
system and no email in this RFC.

## 2. Motivation

**A deployed pipeline fails in silence.** A team running `barca serve` found two jobs that had
failed on every scheduled run for over a week (#434). They found them in Datadog, which they
already had; a team without an observability stack would not have found them at all. Slack is
where a small data team already looks.

**The telemetry layer already does the hard parts.** `crates/barca-core/src/telemetry/mod.rs`
builds one `RunReport` per finished run for every command and every `serve` run, switches
integrations on by name, bounds each delivery to 3 seconds, never fails a run on a delivery
error, and reports a broken backend once when it starts failing and once when it recovers,
not on every run. A Slack integration is one more entry in its `integrations!` table.

**Why not the parked hooks design (#52).** It proposed `[[hooks.on_failure]]` tables in
`barca.toml` with `$VAR` expansion, four event types and two backends. Everything it needs
except the message already exists in the telemetry layer, and it would have put webhook
secrets in a config file that gets committed. It also notified on every failure, which under
`serve` means a message every tick for a job that stays broken.

## 3. Guide-Level Explanation

### 3.1 Turning it on

```bash
export BARCA_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/T000/B000/XXXX
barca serve --timezone utc
```

Creating the webhook is the usual Slack flow: create an app in the workspace, enable incoming
webhooks, add one to a channel, copy its URL. The `telemetry` manual topic carries an app
manifest so this is one paste.

The channel is the one the webhook was created for. To route different projects to different
channels, give each deployment its own webhook.

### 3.2 What gets posted

When `p.py:daily_revenue` fails after succeeding:

```
🔴 p.py:daily_revenue is failing
Step p.py:orders failed after 3 attempts in 4m 12s
KeyError: 'region'
env prod · host barca-prod-1 · run 0179afe0cb58
barca history 0179afe0cb58
```

When it next succeeds:

```
✅ p.py:daily_revenue recovered
Failing since 2026-10-19 06:00 UTC (4 runs) · run 0179b2a1c4d0
```

While it keeps failing, once every 24 hours:

```
🔴 p.py:daily_revenue is still failing
Failing since 2026-10-19 06:00 UTC (96 runs), last error KeyError: 'region'
```

The message names the job (the canonical resolved target, the same as `barca.job` on the
Datadog span), the first failed step, the exception type and the first line of its message,
attempts, duration, env, host and run id, and the command that shows the full run. With
`BARCA_UI_URL` set, the run id links to that run in the web UI. Tracebacks are not sent.

### 3.3 Settings

| Variable | Meaning | Default |
|---|---|---|
| `BARCA_SLACK_WEBHOOK_URL` | Incoming webhook URL. Set and non-empty turns the integration on; unset or empty, it is off and silent. A value that is not an `https://` URL is the usual once-per-process telemetry warning, and nothing is sent. | unset |
| `BARCA_SLACK_NOTIFY` | `changes`: failing, recovered and the daily reminder. `failures`: every failed run, plus recovered. `all`: every finished run that executed at least one step. | `changes` |
| `BARCA_UI_URL` | Base URL of the web UI behind its proxy, for run links. | unset, no link |

### 3.4 HTTP API and UI

No new routes. `barca serve` sends exactly what a CLI run sends, for every run it starts.

## 4. Reference-Level Explanation

### 4.1 Public API Surface

- **Environment:** `BARCA_SLACK_WEBHOOK_URL`, `BARCA_SLACK_NOTIFY` and `BARCA_UI_URL` are new
  variables. They go in `contract.md`'s environment table as experimental. `BARCA_TELEMETRY` is
  unchanged: `slack` is not one of its values, and naming it there gets the existing
  unknown-integration warning plus a hint naming `BARCA_SLACK_WEBHOOK_URL`.
- **stderr:** the existing telemetry warning lines, with `'slack'` as the name. A delivery
  error never includes the webhook URL; the error names only its category (`connection
  failed`, `HTTP 404`, `no reply within 3s`), as the server lifecycle warnings already do.
- **Message text** is not a contract. Its wording and layout may change in any release.
- **No exit code, JSON key or flag changes.** A Slack failure never changes a run's outcome.

### 4.2 What "failing" means

A **job** is the canonical id list `RunReport.job` already carries. Its **state** after a run
is the run's status: `success` or `failed`. `cancelled` and skipped ticks do not change the
state and never post.

With `BARCA_SLACK_NOTIFY=changes`, after each finished run the integration compares the run's
status with the job's previous finished run in the metadata database:

| Previous | This run | Post |
|---|---|---|
| success, or no previous run | failed | is failing |
| failed | failed | is still failing, if the last post for this job was 24 hours ago or more |
| failed | success | recovered |
| success | success | nothing |

A run that executed no steps (every step cached) counts as success and only matters for
recovery.

The previous state comes from the `runs` table, so transitions work the same across separate
CLI processes and across a server restart. The time of the last post per job is kept in a new
`notifications` table (`job`, `integration`, `posted_at`, `state`, `message_ref`), added non-destructively
under the [metadata format policy](https://github.com/barca-orc/barca/blob/main/specs/metadata-schema.md).

`message_ref` is empty for a webhook post, which returns no message id. It is reserved for a
later bot-token mode (section 8) to thread replies under the original failure, so adding that
mode needs no schema change.

### 4.3 Implementation Details

- **Switching on.** `telemetry::configured()` returns the integrations named in
  `BARCA_TELEMETRY`, as today, plus Slack whenever `BARCA_SLACK_WEBHOOK_URL` is set and
  non-empty. Datadog keeps its explicit switch because its settings are Datadog's own `DD_*`
  variables, which a stack sets for other services; the Slack variable is barca's own, so
  setting it is the opt-in.

- `telemetry/slack.rs` implements `Integration::export` and posts one Block Kit message with a
  plain-text fallback.
- **This adds TLS to the binary.** The Datadog integration speaks plain HTTP to a local Agent
  over a hand-written client, so barca has no HTTPS client today. Slack webhooks are HTTPS
  only. The integration uses `reqwest` with `rustls`, the same dependency #394 plans for the
  `--server` client, so the cost is paid once. The implementing PR states the binary-size
  delta, and the `trivial` benchmark must not regress.
- `Integration::export` gains read access to the job's previous finished run and last post.
  That is the only change to the trait; Datadog ignores it.
- Delivery stays inside the 3-second `EXPORT_TIMEOUT`, after the run's results are persisted.
  Slack returns `429` with `Retry-After` when rate-limited; the integration does not wait,
  reports the failure through the existing warning, and does not record the post, so the next
  run retries the transition.
- Message text is capped well under Slack's limits: the exception message's first line, at most
  300 characters.

### 4.4 Rust ↔ Python Boundary

None. Everything happens in the coordinator after the run.

### 4.5 Edge Cases

- **Two jobs over the same failing step** post separately. Each job is what someone scheduled
  or asked for.
- **A partitioned target where some keys failed** is a failed run; the message names the first
  failed key and how many keys failed.
- **A run killed in the middle** has no finished outcome. Reconciliation (#290) later marks it
  `interrupted`; that row is not posted and does not change the job's state. The next finished
  run decides whether anything is posted.
- **Concurrent runs of one job** (a manual `barca get` while a tick runs) can both see the same
  previous state and both post. Rare, and harmless; not worth a lock.
- **A developer's laptop with `BARCA_SLACK_WEBHOOK_URL` exported** posts its own failures. The
  switch is per process; the manual says to set it on the deployment only.
- **A generic `SLACK_WEBHOOK_URL` already in the environment** (CI systems and other tools use
  that name) is ignored. Only the barca-specific name turns the integration on, so it is never
  switched on by accident.

## 5. Determinism, Caching & Testing

No effect on hashing or caching.

Tests, against a fake webhook server like the fake Datadog Agent in
`python/tests/test_telemetry_datadog.py`:

1. Success, then failure, then failure, then success, as four separate CLI processes:
   exactly two posts, `is failing` and `recovered`.
2. Under `serve` with a one-minute schedule and a failing asset: one `is failing` post over
   several ticks; with the reminder interval shortened by a test-only variable, one
   `still failing` post.
3. `BARCA_SLACK_NOTIFY=failures` and `all` post what the table says.
4. A webhook returning `404`, `429` and no reply: the run exits as it would have, one warning
   per outage without the URL in it, and the transition is posted when the webhook recovers.
5. A cached run, a skipped tick and a cancelled run post nothing.
6. The posted body is valid Block Kit with a `text` fallback, and contains no traceback.

## 6. Performance

Off: one environment lookup per run, nothing else. On: one indexed query on
`runs` per finished run, and an HTTP request only on a transition or a reminder. `trivial`
with Slack off must not regress.

## 7. Drawbacks

- A webhook is bound to one channel, so routing by job needs a webhook per deployment.
- The binary gains a TLS stack (shared with #394), which it does not carry today.
- Email and PagerDuty users get nothing from this RFC.
- The 24-hour reminder is fixed in v1.

## 8. Rationale & Alternatives

- **`[[hooks]]` in `barca.toml` (#52).** Rejected: duplicates the telemetry layer, puts secrets
  next to code, and a matrix of events times backends is configuration nobody asked for.
- **A bot token instead of a webhook.** barca is self-hosted, so there is no shared published
  app either way: each team creates a private app in its workspace, and a webhook is that app
  with one permission and one channel. A bot token (`chat:write`) adds posting to any channel
  the bot is invited to (per-job routing), threading reminders and the recovery under the
  original failure, and editing the failure message in place. It costs a more powerful secret
  (any channel the bot is in, instead of one), inviting the bot to channels, and storing each
  post's message id. The stated problem is noticing a broken job, which a webhook post solves;
  threading is polish. Deferred, not rejected: `BARCA_SLACK_TOKEN` and `BARCA_SLACK_CHANNEL`
  can be added later with the same transition logic, using the reserved `message_ref` column,
  and webhook users change nothing.
- **Notify on every failure.** That is `BARCA_SLACK_NOTIFY=failures`; not the default, because
  under `serve` a broken hourly job would post 24 times a day.
- **A generic outgoing webhook.** Useful, but a Slack message is the request; a generic JSON
  webhook can follow as its own integration with the same transition logic.
- **Buttons in Slack (rerun, pause).** They need Slack to call the server, which means
  verifying Slack's signatures inside barca. Authentication is a standing non-goal.

## 9. Prior Art

Dagster's `make_slack_on_run_failure_sensor` posts every failed run, with a filter by job.
Prefect automations post on a flow run state change. Both route through an app token. This
RFC takes Prefect's state-change default and Dagster's single-hook simplicity.

## 10. Unresolved Questions

Decided 2026-10-20 (maintainer, taking each proposal): the variable is `BARCA_SLACK_WEBHOOK_URL`; the default mode is `changes`; the reminder is fixed at 24 hours; no post on an unclean `serve` restart; no job filter in v1; it ships in 0.26.0, ahead of server mode, and whichever of it and #394 lands first brings `reqwest` with `rustls`.

1. **Variable name:** `BARCA_SLACK_WEBHOOK_URL` (proposed), or the generic `SLACK_WEBHOOK_URL`,
   which would also switch it on wherever another tool already set that name.

2. **Default mode:** `changes` (proposed) or `failures`.
3. **Reminder interval:** fixed 24 hours (proposed), configurable, or none.
4. **Server lifecycle:** post when `serve` starts after an unclean stop, or never (proposed:
   never in v1; the Datadog heartbeat covers liveness).
5. **Job filter:** a `BARCA_SLACK_JOBS` glob to post only some jobs, or none in v1 (proposed).
6. **Release:** 0.25.0 alongside the output change (it is additive and independent, but then
   it carries the TLS dependency before #394 does), or 0.27.0 with server mode, where `reqwest`
   lands anyway and the message can say what triggered the run (#393). Proposed: 0.27.0.

## 11. Future Possibilities

- The trigger (`schedule`, `api`, `client`) and scheduled time in the message, once #393
  records them.
- A bot-token mode (`BARCA_SLACK_TOKEN`, `BARCA_SLACK_CHANNEL`) with threads, in-place edits
  and per-job channels.
- A generic `webhook` integration and email through the same transition logic.
