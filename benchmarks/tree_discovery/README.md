# Tree discovery

`barca list` with no file arguments walks the project, keeps the files that import barca, and
parses them into one DAG (issue #202). This benchmark generates a project of 1000 `.py` files
over 70 nested directories: 100 pipelines (two assets each) and 900 helper modules. It compares
discovery against naming the 100 pipeline files explicitly. Nothing executes.

```bash
benchmarks/tree_discovery/bench.sh 20        # BARCA=/path/to/barca, FILES=, PIPELINES= to vary
```

Budget: discovery under 100 ms wall.

Measured 2026-10-04 on an Apple M-series laptop (macOS, release build):

| Command | Mean |
|---|---|
| `barca list` (discovery, 1000 files) | 52.5 ms ± 2.4 ms |
| `barca list` with the 100 files named | 39.3 ms ± 1.2 ms |
