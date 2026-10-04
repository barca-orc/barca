# Examples

Self-contained pipelines. Each topic shows the file and the commands; save the code as
`pipeline.py` in an empty directory and run the commands.

| Topic | What it shows |
|---|---|
| `barca docs examples/duckdb` | A diamond DAG of DuckDB relations, stored as parquet. |
| `barca docs examples/partitions` | Fan-out over keys and fan-in with `collect`. |
| `barca docs examples/deploy-task` | An asset feeding a task, run with `barca run`. |
| `barca docs discovery` ("Cross-file inputs") | A project split into a package of modules, wired by ordinary imports; save each `# file:` block at its path. |

These examples are executed by the test suite, so they stay correct as barca changes.
