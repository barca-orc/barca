---
title: "barca run"
description: "Run tasks. A task always runs; the assets it reads come from cache."
---

Run a task and what it depends on. The task always runs; it is never cached. The assets
upstream of it are served from cache when unchanged, as with `barca get`. `run` takes the same
flags as `get`. In a terminal it prints the task's value under the summary; `-o value` prints
only the value.

```
$ barca run notify pipeline.py
[barca] 1/3 steps | done in 0.0s
Run 14f1e316840c | ran 'notify' in 0.074s (1 step, 1 phase)

Value:
{
  "sent": 80.5
}
```

```bash
barca run deploy pipeline.py --refresh fetch,transform  # recompute these and everything downstream of them
barca run deploy pipeline.py --dry-run --refresh fetch  # preview the cascade
```

To have assets pick up outside data that changed, put a `@sensor` in front of them
(see [Decorators](/reference/api/decorators/#sensor)).

## Several targets

`barca run` and `barca get` take one comma-separated list of targets (no spaces):

```bash
barca run validate_registry,validate_names pipeline.py             # both tasks, one run
barca get summary,orders pipeline.py --dry-run                     # preview the union
```

- The union of the targets' cones is planned once, so a shared upstream step runs (or is served
  from cache) once.
- Every name is checked before anything runs; an unknown name, a task passed to `get`, or an
  empty name (`a,,b`) is a usage error (exit 2).
- Every target runs even if another fails. A failure skips only the steps that depend on it
  (`"status": "skipped"`, reason `upstream_failed`), and the exit code is 1 if any target failed.
- With several targets, `final_output` is replaced by `targets`, keyed by target name in the
  order given:

```json
{"status": "success", "run_id": "...", "elapsed_seconds": 0.2, "steps_executed": 3, "phases": 2, "steps": [...],
 "targets": {"validate_registry": {"status": "success", "final_output": {"models": 2}},
             "validate_names": {"status": "success", "final_output": {"lowercase": true}}}}
```

A failed target is `{"status": "failed", "failed_node": "pipeline.py:...", "error": "..."}`.
`--dry-run` with several targets reports `targets` in place of `target`, each with a `summary`
counted over that target's cone. The full shapes are in `barca docs agents`.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
