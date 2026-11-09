---
title: "Slack Alerts"
description: "Post to Slack when a job starts failing, recovers, or is still failing a day later, with BARCA_SLACK_WEBHOOK_URL."
editUrl: https://github.com/barca-orc/barca/edit/main/crates/barca-cli/docs/slack.md
---

<!-- Generated from crates/barca-cli/docs/slack.md (barca docs slack). Edit that file, then regenerate: BARCA_UPDATE_SNAPSHOTS=1 cargo test -p barca contract:: (or scripts/update-cli-snapshots.sh). -->

Set `BARCA_SLACK_WEBHOOK_URL` to a Slack [incoming webhook](https://api.slack.com/messaging/webhooks)
and barca posts to its channel when a job **starts failing**, when it **recovers**, and **once a
day while it keeps failing**. It never posts for an ordinary success, a fully cached run, a
skipped `barca serve` tick, a cancelled run or a run that was killed. Like Datadog it covers
`barca get`, `barca run` and every run `barca serve` starts.

Slack is the only built-in alert. For email, PagerDuty or anything else, have your own monitor
read run outcomes from `barca history --json`, or from the server's `GET /runs`.

## Turning it on

```bash
export BARCA_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/T000/B000/XXXX
barca serve --timezone utc
```

The variable is the whole switch: Slack is not named in `BARCA_TELEMETRY` (naming it there is
an unknown-integration warning that names this variable). Unset or empty, Slack is off and silent. A
generic `SLACK_WEBHOOK_URL`, which CI systems and other tools set, is ignored. Set the variable
on the deployment only: a laptop with it exported posts its own failures too.

| Variable | Meaning | Default |
|---|---|---|
| `BARCA_SLACK_WEBHOOK_URL` | the incoming webhook URL; it must be `https://`. Any other value is a warning and nothing is sent | unset (off) |
| `BARCA_SLACK_NOTIFY` | `changes`: starts failing, recovered, and the daily reminder. `failures`: every failed run, plus recovered. `all`: every finished run that executed at least one step | `changes` |
| `BARCA_UI_URL` | base URL of the web UI (as reached through its proxy, e.g. `https://barca.example.com/ui/`); the run id in a post links to that run | unset, no link |

## When it posts

A **job** is the canonical resolved target, the same as `barca.job` on a Datadog span:
`nightly` and `pipeline.py:nightly` are one job. After each finished run barca compares its
outcome with the job's previous finished run in the metadata database and with the last post
it made for the job, so it works the same across separate CLI processes and across a server
restart:

| Previous run | This run | `changes` posts |
|---|---|---|
| success, or none | failed | **is failing** |
| failed | failed | **is still failing**, if the last post for the job was 24 hours ago or more |
| failed | success | **recovered** |
| success | success | nothing |

A run where every step was cached counts as a success. A cancelled run, a skipped tick and a
run recorded as `interrupted` do not change the job's state.

## What a post looks like

What a post looks like (the run id links to the web UI when `BARCA_UI_URL` is set):

```
🔴 pipeline.py:daily_revenue is failing
Step pipeline.py:orders failed after 3 attempts in 4m 12s
KeyError: 'region'
env prod · host barca-prod-1 · run 0179afe0cb58
barca history 0179afe0cb58
```

```
✅ pipeline.py:daily_revenue recovered
Failing since 2026-10-19 06:00 UTC (4 runs)
env prod · host barca-prod-1 · run 0179b2a1c4d0
```

A post is one Block Kit message with a plain `text` fallback. It carries the job, the first
failed step (and how many failed), the exception type and the first line of its message (at
most 300 characters), attempts, duration, env, host and run id. Tracebacks are not sent. The
wording is not a contract and may change in any release.

## Delivery

The post goes out after the run's results are recorded, within the same 3-second
limit as every telemetry integration, and never changes the run's outcome or exit code. When
the webhook cannot be reached or refuses the post, stderr gets the usual telemetry warning,
naming the failure category and never the webhook URL:

```
[barca] warning: telemetry 'slack' did not receive run 0179afe0cb58: HTTP 404
```

Categories are `connection failed`, `HTTP <code>` (`HTTP 429 (rate limited)` when Slack
rate-limits), `no reply within 3s` and `request failed`. barca does not wait out a `429`. A post
that was not delivered is not recorded, so the next run of the job posts the transition again.

## Creating the webhook

At [api.slack.com/apps](https://api.slack.com/apps) choose *Create New
App*, then *From a manifest*, pick the workspace and paste:

```yaml
display_information:
  name: barca
  description: Job failures and recoveries from barca
oauth_config:
  scopes:
    bot:
      - incoming-webhook
settings:
  org_deploy_enabled: false
  socket_mode_enabled: false
  token_rotation_enabled: false
```

Then *Install to Workspace*, choose the channel, and copy the webhook URL from *Incoming
Webhooks*. The URL is a secret: anyone holding it can post to that channel, so keep it out of
`barca.toml` and the repository. A webhook posts to one channel; to route projects to different
channels, give each deployment its own webhook.

## Limits

- A run that fails before it starts (a syntax error, an unknown target, a shared-state pull
  that fails) is not a finished run and posts nothing, as with Datadog.
- With shared remote state, the record of the last post travels with the metadata database
  when the run pushes it. A run whose push conflicts and is replayed can lose that record, and
  the next run may then post the same transition again. Two runs of one job finishing at the
  same moment can both post.
- A run without targets is the job `all`, whatever files it reads.
- The reminder interval is fixed at 24 hours. There is no job filter and no per-job channel.
- Runs recorded before this version have no job and do not count as a previous run.

See also: [Telemetry (Datadog)](/reference/telemetry/) (Datadog traces and metrics), [Scheduling With Cron](/scheduling/),
[Operating barca serve](/reference/serve-ops/).

---

*The same text is in your terminal: `barca docs slack`.*
