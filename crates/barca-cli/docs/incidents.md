# Containing an incident: pausing assets

When an asset is failing or producing bad data, an operator holds it back on the server so that
scheduled runs stop computing it (and what reads it) until it is fixed. A pause names a node, a
node and everything below it, or every node of a file. Pauses are experimental (new in 0.41).

```bash
barca pause clean --server                 # scheduled runs skip clean until it is resumed
barca pause clean --downstream --server    # clean and every node below it
barca pause pipeline.py --server           # every node defined in pipeline.py
barca pause clean --downstream --dry-run --server   # pauses nothing: what the pause would cover
barca pause --server --json                # {total, paused: [{id, paused_at, via, vias}], schedules: [{id, paused_at, until}]}
barca resume clean --server                # the next tick computes clean again
barca resume clean --downstream --server   # clean, and the pause of clean --downstream below it
barca resume --all --server                # every paused node, whatever paused it
```

`barca pause` and `barca resume` act on the server named by `--server`, `BARCA_SERVER` or
`[server] url`; without one they are a usage error (exit 2), because only a server runs anything
on a schedule. The same is `GET /pauses`, `POST /pauses` (`{"target", "downstream"}`) and
`POST /pauses/resume` (`{"target", "downstream"}` or `{"all": true}`). A target is named as for
`barca get` (a name, or the full id when the name is ambiguous); one that ends in `.py` names
every node of that file. A `--read-only` server refuses both with `403`.

## What a pause would cover

`barca pause TARGET [--downstream] --dry-run` pauses nothing and prints what the pause would
cover; with `--json`, the answer of `barca pause --json` plus `covers` (`GET
/pauses?target=TARGET&downstream=true`, which a `--read-only` server answers too):

```json
{
  "total": 0,
  "paused": [],
  "schedules": [],
  "covers": {
    "targets": ["pipeline.py:clean"],
    "downstream": ["pipeline.py:report"],
    "schedules": [{"id": "pipeline.py:inbox", "relation": "target"}]
  }
}
```

`targets` are the nodes the target names (one node, or every node of a file) and `downstream`
every node below them, both in dependency order; `downstream` is empty without `--downstream`.
`schedules` are the schedules whose ticks reach a covered node, sorted by id, each with how its
ticks touch what the pause names, the first that applies:

| `relation` | The schedule |
|---|---|
| `self` | is of a node the pause names |
| `target` | has a named node as a target of its ticks: every tick brings that node up to date |
| `input` | reads a named node in its ticks (a target of the tick is below it) |
| `downstream` | reaches none of the named nodes, only nodes below them |

The schedules are found by the scheduler's own walk of the graph (`barca docs scheduling`), so
one is listed exactly when a tick of it would plan a covered node: a schedule above a `Manual`
asset does not reach that asset, only the `Always` nodes below it. It is read from the graph
alone. A node that is pinned or already paused stops a tick's walk at run time, and that is not
accounted for: the list can name a schedule whose ticks are already held back by another pause.
The web UI's "Blast radius" and its incident controls show this answer.

## What a pause does

Pauses are rows of the metadata DB (`asset_pauses`, and `asset_pause_vias` for every pause of
each node), so a restart or a deploy keeps them.

- **A scheduled tick** does not compute a paused node. When the tick's scheduled node, or one of
  the other `Always` nodes the tick reconciles, is paused, its step is `skipped` with reason
  `paused` and a detail naming the command that resumes it. A paused node upstream of a
  node the tick computes is read as it is, like a `Manual` node: its last result is used, and
  it is not recomputed. When a paused node upstream has never been computed, the node that reads
  it cannot run and is skipped with reason `paused` too. A sensor-driven tick is a tick of the
  sensor's schedule and follows the same rule.
- **A request** (`barca get`, `barca run`, `POST /runs`, the web UI's Run button) that names a
  paused node as a target is refused (exit 2, `400` over HTTP) unless it says it means it:
  `--allow-paused` on the command line, `"allow_paused": true` in the body, a confirmation in the
  web UI. A local `barca get` checks the pauses of this machine's metadata DB.
- **After `barca resume`**, the next tick that reaches the node decides it as any other node:
  it is computed when its inputs or code changed while it was paused, and then so are the
  `Always` nodes below it that the tick reconciles. When nothing changed, nothing is
  recomputed. A node read while paused is never recorded as proven fresh, and resuming forgets
  the recorded freshness proofs, so the first run afterwards checks every node against the
  cache once.

```bash
barca get clean pipeline.py --allow-paused   # run it by hand even though it is paused
```

## Several pauses on one node

A node can be held by more than one pause: an operator holds `report`, then an incident pauses
`clean --downstream`, which covers `report` too. The node stays paused until every pause that
holds it is lifted. In `barca pause --json`, `via` and `paused_at` are its earliest pause and
`vias` lists every pause target that holds it, earliest first.

A resume lifts every pause of the nodes it names, and from the nodes below them only its own
pause. `barca resume clean --downstream` resumes `clean` and lifts the pause made by `barca
pause clean --downstream` from each node below it. `report` stays paused, because the
operator's own pause still holds it. A resume answers `removed` (the nodes no pause holds
any more) and `kept` (the nodes in its scope that another pause still holds):

```json
{"removed": ["pipeline.py:clean"], "kept": ["pipeline.py:report"], "total": 1, "paused": [...], "schedules": []}
```

A target matches a pause made with another spelling of it: `clean` and `pipeline.py:clean` are
one target, as are `pipeline.py` and `./pipeline.py`. `barca resume report` names `report`, so
it lifts every pause of `report` and leaves the other nodes of a `--downstream` pause paused.
`barca resume --all` lifts every pause of every node.

Resuming lets the next tick compute the node again. The ticks it skipped while paused are not
caught up. A pause is separate from a schedule's pause (`barca pause --schedule NODE`, `POST
/schedule/{id}/pause`: it holds back one scheduled job's ticks, `barca docs scheduling`, "Pausing
one schedule"; `barca pause` with no target lists both kinds) and from the run queue's pause (`barca queue pause`, which holds
back every queued run); the web UI's asset panel offers all three in one place.

## Known limitations

- A request that does not name the paused node (`barca get` with no target, a run of a file, or
  a run of a node downstream of a paused one) computes it as usual: only scheduled ticks skip a
  paused node that is not a target.
- `--downstream` pauses the nodes below the target when the pause is made. A node added below it
  later is not paused.

See also: `barca docs serve-ops` (the run queue), `barca docs scheduling` (schedules and their pause), `barca docs server-mode`.
