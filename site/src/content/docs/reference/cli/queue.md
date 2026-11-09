---
title: "barca queue"
description: "A server's run queue; pause or resume it (experimental)."
---

A barca server's run queue: the runs executing and the runs waiting to start, in order. It needs
a server (`--server URL`, `BARCA_SERVER` or `[server] url`); without one it exits 2. Experimental.

## Usage

```
barca queue [pause|resume] [--server [URL]] [--json]
```

```bash
barca queue --server                 # running and queued runs (a table in a terminal)
barca queue --server --json          # {paused, paused_at, draining, max_runs, running, queued}
barca queue pause --server           # queued runs wait; runs in flight go on; kept across restarts
barca queue resume --server          # queued runs start again, in order
```

`pause` and `resume` answer with the queue after the change, and exit 2 on a `--read-only`
server. See `barca docs serve-ops`, "The run queue".

With a remote store, `barca serve` backs its metadata DB up every minute it changed (not with
`--read-only`), and `barca state restore` refuses to run while it is up. See
[Remote storage](/reference/remote-storage/).

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
