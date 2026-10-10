---
title: 'RFC-0002: CLI Surface'
description: The barca binary — commands, flags, exit codes, and the stdout/stderr contract.
---

- **Status:** Accepted (retroactive baseline — documents behavior as of v0.6.1)
- **Date:** 2026-07-16
- **Touches:** barca-cli | barca-core
- **Supersedes / Related:** [RFC-0006](/rfcs/0006-configuration-and-remote-state/) (`--env`, config precedence)

---

> **Historical design record:** the accepted baseline below describes the version
> named in its status, with any amendments called out separately. It is not the
> current executable contract. Use the [CLI contract](/reference/cli-contract/),
> [decorator reference](/reference/api/decorators/),
> [server API](/reference/server-api/) and [configuration reference](/reference/config/)
> for implemented behavior. Open proposals remain proposals until separately accepted
> and implemented.

## 1. Summary

`barca` is a single binary with eight subcommands (`get`, `run`, `plan`, `history`,
`stats`, `serve`, `list`, `version`) plus a bare-file shorthand. It is the only
supported way to invoke barca — `python/barca/api.py` shells out to this same binary
rather than reimplementing any logic in Python.

## 2. Motivation

Per [Core Constraints](/core-constraints/) and the project's design principles, planning
must stay in Rust and must never import user code. A CLI-first design means every
consumer — a human at a terminal, `barca.api` shelling out, CI — goes through the exact
same static-analysis-then-execute path, with no separate "programmatic mode" that could
drift from what the CLI does.

## 3. Guide-Level Explanation

### 3.1 CLI

```
barca get [target] <file.py> [file.py ...]   Get asset value(s) — cache-aware
barca run <task> <file.py> [--refresh a,b | --refresh-all]     Run a task (always re-runs)
barca plan <file.py> [file.py ...]           Emit the execution plan as JSON
barca history [-l N]                          Show recent run history
barca stats <target> <file.py> [file.py ...]  Show timing/cache stats for an asset
barca serve [file.py ...] [--port N] [--watch] [--no-schedule] [--timezone TZ]
                                               Run the HTTP API server
barca list <file.py> [file.py ...]            List discovered definitions and their deps
barca version                                 Print version
barca --help                                  Show help
```

`barca pipeline.py` (bare file, no subcommand) is rewritten to `barca get pipeline.py`.

```bash
barca get pipeline.py                 # all assets
barca get summary pipeline.py         # a specific target
barca get pipeline.py --no-cache      # execute everything fresh
barca get pipeline.py --agent         # plain progress lines instead of a progress bar
barca get pipeline.py -o value        # print just the final value (also: json | pretty)

barca run deploy pipeline.py                          # run task + bust all upstream caches
barca run deploy pipeline.py --refresh fetch,transform   # only re-materialize named assets
```

Every command that touches state (`get`, `run`, `plan`, `serve`, `history`, `stats`)
accepts `--env <name>` — see [RFC-0006](/rfcs/0006-configuration-and-remote-state/).

## 4. Reference-Level Explanation

### 4.1 Public API Surface

**Argument dispatch.** If the first positional argument to `get` ends in `.py`, every
argument is treated as a source file (materialize all assets, return the last one's
value). Otherwise the first argument is a target asset/task name and the rest are files.
This ambiguity is intentional shorthand, not accidental — it is what makes the
bare-file rewrite (`barca pipeline.py` → `barca get pipeline.py`) unambiguous with
`barca get target pipeline.py`.

**Exit codes.** `0` on success. `1` on any failure — missing/invalid files, parse
errors, DAG validation errors (e.g. the cache-poisoning guard from
[RFC-0001](/rfcs/0001-node-kinds-and-freshness/)), or a failed step during execution.
There are no distinct exit codes per failure class; failure detail lives in the stderr
message and, for `get`/`run`/`plan`, in the JSON on stdout.

**stdout/stderr contract.** Usage and argument errors go to stderr as plain text
(`error: ...\n\nUsage: ...`) and never touch stdout. `get`/`run`/`plan` write a single
JSON object to stdout as their last line (`plan` may pretty-print the whole output as
JSON; `get`/`run` may interleave user `print()` output before the final JSON line) —
this is the exact contract `python/barca/api.py`'s `_exec()` parses: try the full
stdout as JSON, fall back to the last line. Any consumer scripting against `barca` must
follow the same rule: **read the last line, not all of stdout, as the result.**

**`get` result shape** (also what `POST /run` resolves to, see
[RFC-0004](/rfcs/0004-http-server-api/)):

```json
{
  "run_id": "1b9422cf12f3",
  "elapsed_seconds": 0.115,
  "steps_executed": 2,
  "phases": 1,
  "final_output": { "path": ".barca/artifacts/…", "format": "json", "size_bytes": 8 }
}
```

**Output flag (`-o`).** `get`/`run` accept `-o value | json | pretty` to control how
`final_output` is rendered — `value` prints just the deserialized value, the others
print the full JSON envelope.

**`serve`.** See [RFC-0004](/rfcs/0004-http-server-api/) for the full endpoint surface;
this RFC only owns the CLI flags (`--port`, `--watch`, `--no-schedule`, `--timezone`).

**`history`/`stats`.** Human-readable table output (`history [-l N]`,
`stats <target> <files>`). `python/barca/api.py` parses these tables back into dicts —
see `history()`/`stats()` in `api.py` — which makes their column layout part of the
load-bearing surface even though it's rendered as a table, not JSON. A column reorder
in `barca-cli`'s output would silently break `barca.history()`/`barca.stats()`.

### 4.2 Implementation Details

`barca-cli` (`crates/barca-cli/src/main.rs`) is a thin `clap` dispatcher: it parses
arguments and calls straight into `barca_core::commands::*`
(`crates/barca-core/src/commands.rs`), which does the real work (parse → DAG → plan →
dispatch → persist). `serve` is the one subcommand that hands off to a different crate
(`barca-server`) instead of a `commands::*` function — see
[RFC-0004](/rfcs/0004-http-server-api/).

### 4.3 Rust ↔ Python Boundary

Not directly touched by CLI argument parsing — the CLI's job ends at calling into
`barca-core`, which owns the worker subprocess boundary (see the io_loop/protocol
internals in [Architecture](/architecture/)). The one place a Python process observes
the CLI directly is `python/barca/api.py._find_binary()`, which locates the `barca`
binary next to `sys.executable` (same venv) or on `PATH`, and warns (not errors) on a
`barca --version` mismatch against the installed `barca` Python package.

### 4.4 Node-Kind Semantics

Unaffected — the CLI has no node-kind-specific behavior beyond what's already described
in [RFC-0001](/rfcs/0001-node-kinds-and-freshness/) (`get` targets assets/sensors,
`run` targets tasks).

### 4.5 Edge Cases

- `barca get` on a target that resolves to a task fails — `get` is for cache-aware
  asset retrieval; use `run` for tasks.
- `--refresh` on `run` is meaningless without naming actual upstream assets in
  the task's dependency cone; unknown names are a plan-time error.

## 5. Determinism, Caching & Testing

Not applicable beyond what RFC-0001/RFC-0005 already cover — the CLI is a pass-through
to `barca-core`'s planning and caching. Covered by
`tests/integration/*.sh` (shell-based CLI integration tests run in CI against the built
wheel) plus `cargo test` for `barca-cli`'s argument parsing.

## 6. Performance

The CLI itself is the thing being measured in `benchmarks/` — process startup +
arg-parse + dispatch is part of the ~4ms fixed Rust overhead cited in
[Architecture Decisions](/architecture-decisions/) §5. Any change to argument parsing
or dispatch should be checked against `benchmarks/trivial/bench.sh`.

## 7. Drawbacks

The `get`-vs-target-vs-file positional ambiguity (§4.1) is a small but real footgun —
`barca get foo.py bar` and `barca get bar foo.py` mean different things depending on
which argument ends in `.py`.

## 8. Rationale & Alternatives

A `--target`/`--file` flag pair (rejected) would remove the positional ambiguity but
adds ceremony to the common case (`barca get pipeline.py`) that this design optimizes
for. The bare-file shorthand (`barca pipeline.py`) was kept for the same reason —
lowest-friction path from "I have a pipeline" to "I have a value."

## 9. Prior Art

`dagster asset materialize`, `prefect deployment run` — both require more ceremony
(deployment registration, explicit flow references) than barca's direct
file-as-argument model. See [Framework Comparison](/comparisons/framework-comparison/).

## 10. Unresolved Questions

Should `history`/`stats` gain a `--json` flag so `barca.api` can parse structured
output instead of scraping table columns (§4.1)?

## 11. Future Possibilities

The `barca prune` command described as intent in
[Core Constraints](/core-constraints/) — reclaiming disk from history unreachable from
the current DAG — does not exist yet and would need its own RFC before implementation
(destructive, opt-in operation).
