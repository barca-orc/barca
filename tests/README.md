# tests/

The test suites that are not next to the code they test. The Rust tests live in `crates/*`
and the Python tests in `python/tests/`; see `contributing/testing.md` on the site
(`site/src/content/docs/contributing/testing.md`) for every suite and what CI runs.

## integration/

Shell tests of the installed `barca` binary: `tests/integration/test_*.sh`, each a standalone
script that builds a temporary project, runs barca on it and checks output, exit codes and
files. They run `.venv/bin/barca` when it exists, else the `barca` on `PATH`.

```bash
bash tests/integration/test_cli.sh
```

CI runs them in the full tier (a release PR, its merge group, or a PR labelled `ci-full`):
the `integration` job runs the cache, CLI, env, partitions, remote-state and run-refresh
scripts and the benchmark examples (`test_benchmark_examples.sh`), and the `proxy` job runs
`test_reverse_proxy.sh` and `test_server_mode.sh` (they need nginx). Every script here must be
named in `.depot/workflows/ci.yml`: `python/tests/test_ci_coverage.py` fails otherwise.

## simulation/

`tests/simulation/sim.py` compresses long use of a real `barca serve` under faults: a writer
server with a scheduled task beside a `--read-only` server on the same `.barca`, a seeded
generator that edits the source, kills servers and workers, deletes artifacts and runs
`barca gc`, and invariants checked after every operation (values, health, integrity, history,
liveness, eviction, size). The seed fixes the sequence of operations, not their timing.

```bash
tests/simulation/sim.py --seed 7 --seconds 120
tests/simulation/sim.py --seeds 20 --seconds 60
```

It is too slow for any CI tier. `python/tests/test_simulation.py` runs one short seed and
proves each invariant can fail (`--sabotage`) in CI's full tier; `.github/workflows/simulation.yml`
runs the long version weekly (Monday 03:17 UTC, or on demand with `workflow_dispatch`,
`seeds` input): 5 seeds of 10 minutes each against a 2 GB seed database, with
`--strict-reads`, and uploads a failed run's `trace.jsonl`, logs and `pipeline.py` as the
`simulation-failure` artifact. Exit codes: 0 every seed passed, 1 an invariant failed, 3 the
harness itself failed.
