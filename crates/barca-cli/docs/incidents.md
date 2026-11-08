# Containing an incident: pausing assets

When an asset is failing or producing bad data, an operator holds it back on the server so that
scheduled runs stop computing it (and what reads it) until it is fixed. A pause names a node, a
node and everything below it, or every node of a file. Pauses are experimental (new in 0.41).

```bash
barca pause clean --server                 # scheduled runs skip clean until it is resumed
barca pause clean --downstream --server    # clean and every node below it
barca pause pipeline.py --server           # every node defined in pipeline.py
barca pause --server --json                # {total, paused: [{id, paused_at, via}]}
barca resume clean --server                # the next tick computes clean again
barca resume clean --downstream --server   # clean and every node below it
barca resume --all --server                # every paused node
```

`barca pause` and `barca resume` act on the server named by `--server`, `BARCA_SERVER` or
`[server] url`; without one they are a usage error (exit 2), because only a server runs anything
on a schedule. The same is `GET /pauses`, `POST /pauses` (`{"target", "downstream"}`) and
`POST /pauses/resume` (`{"target", "downstream"}` or `{"all": true}`). A target is named as for
`barca get` (a name, or the full id when the name is ambiguous); one that ends in `.py` names
every node of that file. A `--read-only` server refuses both with `403`.

## What a pause does

Pauses are rows of the metadata DB (`asset_pauses`), so a restart or a deploy keeps them.

- **A scheduled tick** does not compute a paused node. When the tick's scheduled node, or one of
  the `Always` nodes below it that the tick reconciles, is paused, its step is `skipped` with
  reason `paused` and a detail naming the command that resumes it. A paused node upstream of a
  node the tick computes is read as it is, like a `Manual` node: its last result is used, and
  it is not recomputed. When a paused node upstream has never been computed, the node that reads
  it cannot run and is skipped with reason `paused` too. A sensor-driven tick is a tick of the
  sensor's schedule and follows the same rule.
- **A request** (`barca get`, `barca run`, `POST /runs`, the web UI's Run button) that names a
  paused node as a target is refused (exit 2, `400` over HTTP) unless it says it means it:
  `--allow-paused` on the command line, `"allow_paused": true` in the body, a confirmation in the
  web UI. A local `barca get` checks the pauses of this machine's metadata DB.

```bash
barca get clean pipeline.py --allow-paused   # run it by hand even though it is paused
```

Resuming lets the next tick compute the node again. The ticks it skipped while paused are not
caught up. A pause is separate from a schedule's pause (`POST /schedule/{id}/pause`, which holds
back one scheduled job's ticks) and from the run queue's pause (`barca queue pause`, which holds
back every queued run); the web UI's asset panel offers all three in one place.

## Known limitations

- A request that does not name the paused node (`barca get` with no target, a run of a file, or
  a run of a node downstream of a paused one) computes it as usual: only scheduled ticks skip a
  paused node that is not a target.
- `--downstream` pauses the nodes below the target when the pause is made. A node added below it
  later is not paused.

See also: `barca docs serve-ops` (the run queue), `barca docs scheduling` (schedules and their pause), `barca docs server-mode`.
