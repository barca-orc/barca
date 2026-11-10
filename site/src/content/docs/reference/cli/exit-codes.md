---
title: Exit Codes and Errors
description: "What barca's exit codes 0, 1, 2, 3 and 130 mean, the JSON error envelope on stderr, and how a script or agent should read a failure."
---

| Code | `kind` | Meaning |
|---|---|---|
| 0 | | Success |
| 1 | `step_failed` | A step of yours raised. The traceback is included and the run is recorded as failed |
| 2 | `usage` | Bad flags or arguments, an unknown target, `get` on a task or `run` on an asset, an unreadable or invalid `.py` file, an invalid `--env` or barca.toml |
| 3 | `infra` | Barca or its environment failed: metadata DB, worker pool, remote state, I/O |
| 130 | `cancelled` | Ctrl-C or SIGTERM |

The same table, generated from the code, is in the [CLI contract](/reference/cli-contract/#exit-codes).

A closed stdout or stderr never makes barca panic, never stops a run and is not an error:
output for the closed stream is dropped, a `get` or `run` finishes and is recorded, and the
exit code is the one the command would have had with a reader (`barca list | head -1` exits 0).
See [the CLI contract](/reference/cli-contract/#a-closed-stdout-or-stderr-stable).

An error in a pipeline file fails every command that reads the file with exit 2, whatever the
target: a syntax error, or a decorator called with an argument it does not define
(`@asset(after=other)`, `input=` for `inputs=`). See
[Accepted arguments](/reference/api/decorators/#accepted-arguments). A syntax error in a helper
module a pipeline file imports fails, with exit 2 before anything runs, every command that plans
a step of that file (`plan`, `status`, `get`, `run`, `--dry-run`), naming the module, line,
column and source line; `list` and steps of other files are not affected.

In JSON output mode (whenever results are JSON: piped or captured stdout, `--json`, `-o json` or
`BARCA_OUTPUT=json`; `plan` always; `docs` with `--json`), an error is a single JSON line, the
last JSON error object on stderr (a failed run adds its summary afterwards):

```
{"code":2,"error":"Asset 'nope' not found. Available: pipeline.py:orders, pipeline.py:total, pipeline.py:notify","kind":"usage","remediation":"Run `barca list pipeline.py` to see available assets and tasks."}
```

`error`, `code`, `kind` and `remediation` are always present. When a step fails (`kind:
"step_failed"`) the envelope also has `node` (the failing step's id), `traceback` (the Python
traceback of your code, with barca's own frames removed), `artifact_dir` (where that step's
artifacts are stored; a local path or remote URI, which may not exist if the step never
succeeded) and `phase` (where the step failed: `user_code` for your code; `load_inputs`,
`split_output`, `write_output` or `sink` for barca's own work around it, whose traceback keeps
barca's frames and whose prose says `failed in barca`; `null` when unknown, such as a worker
that died):

```
{"artifact_dir":".barca/artifacts/pipeline.py--clean","code":1,"error":"step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero","kind":"step_failed","node":"pipeline.py:clean","phase":"user_code","remediation":"Fix the error in 'pipeline.py:clean' (see the traceback) and re-run the same command. Steps that succeeded are cached and will not re-run.","traceback":"  File \"/home/you/project/pipeline.py\", line 11, in clean\n    return x / 0\n           ~~^~~"}
```

In human mode (a terminal, `--pretty`, `-o pretty`, `-o path` or `-o value`) the error is prose, with the remediation
on the last lines. Errors never go to stdout. The Python API raises `barca.BarcaError` with the
same fields as attributes (`kind`, `code`, `remediation`, `node`, `traceback`, `artifact_dir`,
`phase`).

When a step fails in JSON mode, `get`/`run` still print one result line on stdout, so you can
read the outcome without parsing stderr: `{"status": "failed", "failed_node": ..., "error": ...,
"run_id", "steps", ...}`, where the failed step's `status` is `failed`. A successful result has
`"status": "success"`. After the error, stderr ends with one greppable line:
`[barca] run failed: step 'pipeline.py:clean' failed: ZeroDivisionError: division by zero; 0 downstream steps skipped (exit 1)`.

### A worker that dies

A worker process that dies while it runs a step (the kernel's out-of-memory killer, a segfault
in a C extension, `os._exit()` in your code) fails the step with exit 1, and the error names
how the process ended, in the result JSON, the envelope and the `run failed:` line:

```
worker for pipeline.py:train exited with SIGKILL (possibly the OOM killer)
worker for pipeline.py:train exited with SIGSEGV
worker for pipeline.py:train exited with code 3
```

SIGKILL is what the out-of-memory killer sends: check the container's or host's memory limit.
Several heavy steps running at once in separate workers add up; give them a `weight=` so fewer
run at once (see [Decorators](/reference/api/decorators/#heavy-steps-weight)). A worker frozen waiting for
its `parallel()` branches adds `while it waited for its parallel() branches`. When the exit
status is not available within a few hundred milliseconds, the error is the plain
`worker disconnected`.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [CLI contract](/reference/cli-contract/)
