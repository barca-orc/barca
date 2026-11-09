---
title: "barca pin and unpin"
description: "Hold an asset at an earlier result or supplied data, and release it (experimental)."
---

Roll an asset back to an earlier materialization: every run (downstream steps, `barca get`,
`barca.get()`, schedules) reads the pinned result and nothing recomputes the asset until
`barca unpin`. Experimental; the full semantics are in [Pinning and Rollback](/reference/pins/).

```bash
barca pin orders --versions          # recent successful materializations to choose from
barca pin orders --previous          # roll back one version
barca pin orders --from-file fix.json   # or override it with data you supply
barca unpin orders                   # back to normal
```

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
