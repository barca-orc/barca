---
name: barca
description: Drive barca, the embedded Python asset orchestrator, from the command line. Use when a project has Python files with @asset, @task or @sensor decorators (from barca import ...), a barca.toml, or a .barca/ directory, or when asked to run, refresh, debug or inspect a barca pipeline or its cached outputs. Covers discovering nodes (barca list, barca status), running them (barca get for assets, barca run for tasks), previewing with --dry-run, the target-before-files argument order, comma-separated targets and --refresh, exit codes, and the JSON on stdout / error envelope on stderr contract.
---

# barca for agents

barca runs Python functions decorated with `@asset` (cached), `@task` (always runs) and
`@sensor`. It reads the source statically, runs only what is stale, and stores every output
under `.barca/`. Ask the `barca` CLI what exists, what is cached and what ran. Full reference:
`barca docs agents`.

## The loop

```bash
barca list pipeline.py                  # discover: every node, its kind, inputs, env
barca status pipeline.py                # cached/stale/never_run and why, last run, rows/columns
barca get total pipeline.py --dry-run   # preview: what would run or come from cache; writes nothing
barca get total pipeline.py             # execute an asset and its upstream cone
barca run report pipeline.py            # execute a task (always re-runs; upstream assets cached)
```

- `get` is for assets, `run` for tasks; the wrong one exits 2 and names the right one.
- A second identical `get` reports `steps_executed: 0` (all cached).
- Any directory inside the project works: barca runs from the nearest `barca.toml` above you
  and shares its `.barca/` cache. File arguments are relative to where you are.
- Unsure of a name? `barca list <files>`. An unknown name exits 2 and lists the valid ones.

## Argument order: target, then files

`barca get total pipeline.py other.py`. With no target, `barca get pipeline.py` materializes
every asset and sensor in the file and skips tasks (previously it ran tasks too); stderr
names the skipped tasks. A file with only tasks gets nothing (exit 0, empty `steps`); run a
task with `barca run <task> <files>`. `barca run pipeline.py report` exits 2, prints the
corrected command (`barca run report pipeline.py`) and runs nothing.

## Several targets, refresh, cascade

Lists are comma-separated with no spaces:

```bash
barca get src,total pipeline.py                   # one run; shared upstream runs once
barca run report pipeline.py --refresh src,clean  # re-run these and everything downstream
barca run report pipeline.py --refresh clean --no-cascade   # only clean
barca get total pipeline.py --refresh-all         # recompute an asset's whole cone
```

- `--refresh a b` is an error that tells you to write `--refresh a,b`.
- `--refresh` cascades: downstream assets re-run too (reason `refresh_cascade`). With
  `--no-cascade` they stay cached, do not reflect the refresh, and barca warns.
- `get` and `run` take the same `--refresh`, `--no-cascade` and `--refresh-all`. `--no-cache` is
  a deprecated spelling of `--refresh-all` (it warns); do not use it.
- Data that changes in place (a blob overwritten at the same path): an asset that reads a
  `@sensor` re-runs when the sensor's returned value changes, so have a sensor return the etag.
  `--dry-run` and `status` assume the sensor returns its last value; before it ever ran, its
  consumers are `unknown` (reason `sensor_output_unknown`). See `barca docs cache`.
- Several targets: the result has `targets` (per name: `status`, then `final_output`, or
  `failed_node` and `error`) instead of `final_output`. Every target runs even if another fails.

## Output contract

- **stdout** is the result: JSON whenever stdout is not a terminal; pass `--json` in scripts
  anyway. `get`/`run` print one line: `status` (`success`/`failed`), `run_id`,
  `steps_executed`, `steps` (each `ran`/`cached` and why), `final_output` (for parquet/pickle a
  pointer, `{"_barca_artifact": {"path", "format", "size_bytes"}}`).
- Your steps' own `print` output goes to stderr, so stdout is only the result.
- **stderr** has progress and errors. In JSON mode its **last line** is the error envelope
  `{"error", "code", "kind", "remediation"}`, plus `node`, `traceback`, `artifact_dir` for a
  failed step. `remediation` is usually the command to run next.

| Exit | `kind`        | Meaning                                         | Do                   |
|------|---------------|-------------------------------------------------|----------------------|
| 0    |               | success                                         |                      |
| 1    | `step_failed` | your code raised (stdout still has the result)  | fix the code, re-run |
| 2    | `usage`       | bad arguments or names; nothing ran             | fix the command      |
| 3    | `infra`       | barca or its environment (DB, workers, I/O)     | retry                |
| 130  | `cancelled`   | interrupted                                     | re-run               |

Steps that finished before a failure or cancel stay cached: re-running resumes.

## Agent flags and jq

```bash
barca get total pipeline.py --agent                     # plain progress lines on stderr
barca get total pipeline.py --fields id,status,reason   # trim each entry of `steps`
barca list pipeline.py --fields id,kind,inputs          # trim each node
```

- `--fields` implies JSON; an unknown key is an error listing the valid ones.
- `list` and `status` (100 nodes) and `history` (10 runs) are bounded; their JSON has `total` and
  `truncated`. Pass `--limit N` or `--all` for more.
- Before piping into `jq`, `set -o pipefail`, or the pipeline exits 0 when barca failed:

```bash
set -o pipefail
barca run report pipeline.py | jq '.status'
```

## Guardrails

- **Never edit or delete anything in `.barca/`** or query its DB; barca owns it. Reading an
  artifact path barca printed (a `final_output` pointer, `cache.artifact` in `status`) is fine.
- **Never import the user's modules** to inspect outputs or predict a run: importing executes
  their code. Use `barca status total pipeline.py --sample 3` (state, artifact path, shape, sample
  rows) and `--dry-run`. In Python, `barca.get("total", "pipeline.py")` loads a value.
- Never bust the cache by hand; use `--refresh` or `--refresh-all`. After a failure, fix the code
  and re-run the same command.

## One way per task

| Task                              | Command                                    |
|-----------------------------------|--------------------------------------------|
| What exists?                      | `barca list <files>`                       |
| What state is it in?              | `barca status [target] <files>`            |
| What would run?                   | add `--dry-run`                            |
| Asset value / run a task          | `barca get <asset> <files>` / `barca run <task> <files>` |
| Re-run upstream assets            | `barca run <task> <files> --refresh a,b`   |
| Pick up data changed in place     | a `@sensor` returning its etag, read by the asset |
| Past runs                         | `barca history`                            |
| Flags / concepts                  | `barca <command> --help` / `barca docs <topic>` |

Use these spellings. Older ones still parse for compatibility (`-o json`, `barca pipeline.py`
for `barca get pipeline.py`); don't use them.
