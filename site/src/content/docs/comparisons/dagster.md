---
title: "Barca vs Dagster"
description: Install size, steps to a first result, run output and error output for the same two-asset pipeline, measured on 2026-06-10 with barca 0.2.0 and Dagster 1.13.8.
---

Last measured: 2026-06-10, with barca 0.2.0 and Dagster 1.13.8, on macOS (Apple Silicon), Python 3.14, both installed from PyPI with `uv`. Not re-run since; the current barca release is 0.18.0. Re-run tracked in [#277](https://github.com/barca-orc/barca/issues/277).

Everything on this page describes those two versions on that date unless a note says otherwise.
Dagster has had releases since, and so has barca: where barca's behavior has changed, a note
dated 2026-10-07 says how.

## Install footprint

| Metric | barca 0.2.0 | Dagster 1.13.8 |
|--------|-------|---------|
| Packages installed | 1 | 100 |
| Venv size | 16 MB | 301 MB |
| Lockfile entries | 2 | 109 |
| Wheel download | 6.9 MB | 30+ MB |

The barca wheel contained a Rust binary and Python decorator stubs and declared no dependencies.
The Dagster scaffold installed 100 packages, among them cryptography, uvloop, sqlalchemy, grpcio,
graphql, starlette, pydantic and protobuf.

Note, 2026-10-07: the barca wheel now also embeds the web UI that `barca serve` shows. barca
0.18.0 is still one package with no dependencies; its macOS arm64 wheel is 9.8 MB on PyPI, and a
fresh Python 3.12 virtualenv with only barca installed is 21 MB (`du -sh`). The Dagster column
was not re-measured.

## Steps to first output

### Barca

```bash
uv add barca                    # install
cat > pipeline.py << 'PY'       # write code
from barca import asset

@asset()
def raw_data() -> list[dict]:
    return [{"x": 1}, {"x": 2}, {"x": 3}]

@asset(inputs={"data": raw_data})
def summary(data: list[dict]) -> dict:
    return {"count": len(data), "total": sum(d["x"] for d in data)}
PY
barca get pipeline.py            # run
```

These three steps still work on barca 0.18.0.

### Dagster

The steps of the Dagster quickstart as followed on 2026-06-10:

```bash
uvx create-dagster@latest project myproj   # scaffold (interactive prompt)
cd myproj && source .venv/bin/activate     # enter project
dg scaffold defs dagster.asset assets.py   # generate boilerplate
# edit src/myproj/defs/assets.py           # write code
uv add pandas                              # install deps for the quickstart
dg launch --assets my_asset                # run
```

The quickstart also had the reader create a data directory and a CSV file before the asset could
run.

## Running the same two-asset pipeline

| Metric | barca 0.2.0 | Dagster 1.13.8 |
|--------|-------|---------|
| Command | `barca get pipeline.py` | `dg launch --assets raw_data,summary` |
| Total time | 240ms | 1,550ms |
| Output lines | 2 (progress + JSON) | 29 (all DEBUG) |
| Result access | JSON on stdout | Pickled to temp dir |
| Error output | 1 line | 25+ lines with internal frames |

Each time is one run, not an average.

### Barca output

```
[barca] 2/2 steps done in 0.0s
{"elapsed_seconds":0.241,"final_output":{"count":3,"total":6},"phases":1,"run_id":"...","steps_executed":2}
```

Note, 2026-10-07: barca 0.18.0 prints more keys in this JSON object (`status`, a `steps` array
with each step's run hash and cache status, and `warnings`). The current shape is in the
[CLI contract](/reference/cli-contract/).

### Dagster output (abridged from 29 lines)

```
2026-06-10 ... - dagster - DEBUG - RUN_START - Started execution of run for "__ASSET_JOB".
2026-06-10 ... - dagster - DEBUG - ENGINE_EVENT - Executing steps using multiprocess executor
2026-06-10 ... - dagster - DEBUG - raw_data - STEP_WORKER_STARTING - Launching subprocess
... (12 more lines for raw_data) ...
2026-06-10 ... - dagster - DEBUG - summary - STEP_WORKER_STARTING - Launching subprocess
... (12 more lines for summary) ...
2026-06-10 ... - dagster - DEBUG - ENGINE_EVENT - parent process exiting after 1.55s
2026-06-10 ... - dagster - DEBUG - RUN_SUCCESS - Finished execution of run for "__ASSET_JOB".
```

## Error output

The same asset raising `ValueError("something went wrong")`.

### Barca 0.2.0

```
[barca] 0/1 steps done in 0.0s
Worker failed: something went wrong
```

Note, 2026-10-07: barca 0.18.0 prints more than this. Piped, the same failure gives a JSON
result on stdout with `"status": "failed"`, `"error"` and `"failed_node"`, and on stderr a
`[barca] run failed: step 'broken.py:oops' failed (exit 1)` line followed by one JSON line that
carries the error, a remediation, and the traceback frames from the user's file. The exit code
is 1. See [the CLI contract](/reference/cli-contract/).

### Dagster 1.13.8

```
dagster._core.errors.DagsterExecutionStepExecutionError: Error occurred while executing op "oops"::
ValueError: something went wrong

Stack Trace:
  File ".../dagster/_core/execution/plan/utils.py", line 57, in op_execution_error_boundary
    yield
  File ".../dagster/_utils/__init__.py", line 394, in iterate_with_context
    next_output = next(iterator)
  File ".../dagster/_core/execution/plan/compute_generator.py", line 136, in _coerce_op_compute_fn_to_iterator
    ...
  File "broken.py", line 5, in oops
    raise ValueError("something went wrong")
```

The frame from the user's file is the last one, below the Dagster frames.

## Other things observed in the same session

These are notes from using Dagster 1.13.8 on 2026-06-10. They have not been checked against a
later Dagster release.

- `dg list defs` printed a table of every asset with its deps, group and kind, without running
  anything. barca's equivalent is `barca list`, added in 0.2.1.
- The Dagster scaffold included a `.gitignore` covering the files Dagster writes. barca 0.2.0
  created `.barca/` without one. Note, 2026-10-07: barca 0.18.0 writes `.barca/.gitignore`
  containing `*`, so the directory ignores itself.
- A `.py` file placed in the scaffold's `defs/` folder was picked up without registration.
  barca 0.2.0 needed the files named on the command line. Note, 2026-10-07: barca 0.18.0 reads
  every `.py` file under the project root that imports barca when no file is named; see
  [Discovery](/reference/discovery/).
- The Dagster project needed a `pyproject.toml`, a `src` layout, `definitions.py` and a `defs/`
  folder before `dg launch` ran. `barca get file.py` runs on a single file.
- Each asset printed 14 DEBUG lines per run. No option to reduce this was found during the test.
- `create-dagster` asked "Run uv sync? (y/n)". No `--yes` flag was found during the test.
- `dg` commands printed a five-line warning when the project's virtualenv was not activated,
  including when run as `.venv/bin/dg`.
- Both a `dagster` command and a `dg` command were installed.
