---
title: "barca plan"
description: "Print the execution plan as JSON without running anything (experimental)."
---

The document includes `plan_version: 1`, independent of the package and database versions.
Consumers of saved plans should reject unsupported versions.

Parse the source files and print the execution plan as JSON, without running anything.
The pool size matches execution: a positive `BARCA_POOL_SIZE`, else `[execution] pool_size`, else
the available cores, or the capacity when it is smaller; `pool_size` and `pool_source` say which.
Experimental: the layout follows the planner and may change between releases. Each phase's
`reason` is `{"type": "initial"}` or `{"type": "fan_in", "node_id": "..."}`. Planning reads no
state, so `plan` takes no `--env` and does not say what is cached; use `--dry-run` or
`barca status` for that. Its only option is `--help`.

```
$ barca plan pipeline.py
{
  "plan_version": 1,
  "total_steps": 3,
  "phases": [
    {
      "reason": {
        "type": "initial"
      },
      "streams": [
        {
          "stream_id": "p0-w0",
          "steps": [
            "pipeline.py:orders",
            "pipeline.py:total",
            "pipeline.py:notify"
          ]
        }
      ]
    }
  ],
  "warnings": []
}
```

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
