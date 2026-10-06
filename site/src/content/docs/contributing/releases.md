---
title: Releases
description: What shipped in each barca release, and what's scoped for the future.
---

This page scoped barca by release through 0.6 so the scope did not quietly expand. It is not kept up to date: for what shipped in every release since, see the [Changelog](/contributing/changelog/). The "Planned" and "Future" lists below are the old plans, not a current roadmap; open issues are.

## 0.1.x (shipped)

The Rust rewrite. Replaced the pure-Python prototype with a hybrid Rust+Python
architecture: Rust parses, plans, and dispatches; Python executes.

Shipped:

- `@asset` with dependency tracking (`inputs=`), caching (`run_hash`), partitions
- `@sensor` for external state observation
- `@task` for side-effect operations (always re-runs, never cached)
- `@sink` for writing outputs to disk
- `partitions()`, `partitions_from()`, `collect()` for partition workflows
- Static analysis via ruff (never imports user code)
- Turso/libSQL for persistence (`.barca/metadata.db`)
- CLI: `barca get`, `barca run`, `barca plan`, `barca history`, `barca stats`
- `barca serve` HTTP API (axum)
- Retries with backoff (`retries=`, `retry_backoff=`)
- Benchmarks: 13-97x faster than Dagster/Prefect across all workloads

## 0.2.0 (shipped)

The execution engine rewrite. Replaced the old per-thread dispatch system with
a stateless worker pool coordinated via Unix domain sockets.

Shipped:

- **UDS coordination protocol**: length-prefixed JSON over Unix domain sockets,
  290K msg/s at 128 workers
- **Stateless workers**: receive one task at a time from Rust via a global ready queue.
  No pre-assigned queues, no head-of-line blocking.
- **`parallel()` and `parallel_map()`**: dynamic fan-out at runtime via
  SIGSTOP/SIGCONT. Scales to 1000+ items, nested parallel works recursively.
- **Type-safe coordinator**: `load_phase()` consumes the planner's Phase directly.
  StepId on every Item. Step accounting invariant (assert on count mismatch).
- **orjson** optional dependency for faster Python serialization
- **-4000 lines net**: removed executor.rs, scheduler.rs, work_plan.rs, old dispatch loop

Issues closed: [#70](https://github.com/ExSidius/barca/issues/70) (UDS communication),
[#58](https://github.com/ExSidius/barca/issues/58) (asset vs task model)

## 0.3.0 (shipped)

Goal: scheduling, remote I/O, observability.

Delivered:

- **Cron scheduling enforcement** — `barca serve` fires `Schedule("0 5 * * *")`
  assets, sensors, and tasks on their cron tick
  ([#54](https://github.com/ExSidius/barca/issues/54)). Includes:
  - **Durable catch-up** — last-fire times persist; a job whose tick passed
    during downtime fires once on restart.
  - **Configurable timezone** — `--timezone local|utc|<IANA>`.
  - **Parallel runs** — independent runs execute concurrently (bounded pool),
    with DB writes serialized; a job never overlaps itself.
  - **Observability** — `GET /schedule` and `barca list <files>` (next fire times);
    live reload under `--watch`.
- **Python server client** — `barca.Client` (stdlib-only) to trigger runs, poll
  status, and inspect schedules over the HTTP API.

## 0.4.0 (shipped)

Goal: remote artifact storage and execution reliability.

Delivered:

- **Remote artifact backends** — the artifact store and `@sink` destinations
  accept object-store URIs via fsspec scheme dispatch: Azure ADLS Gen2
  (`abfs://`/`abfss://`) first-class, S3 and GCS pluggable
  ([#55](https://github.com/ExSidius/barca/issues/55)). Includes:
  - **Optional extras** — `barca[azure]`, `barca[s3]`, `barca[gcs]`,
    `barca[remote]`; the core install stays zero-dependency.
  - **`BARCA_ARTIFACT_URI`** — points the primary artifact store at a remote
    prefix; `BARCA_STORAGE_OPTIONS` passes per-protocol fsspec options.
    Credentials use each backend's native default chain.
  - **Staged writes** — serializer → local temp file → atomic rename (local)
    or chunked upload (remote); large payloads are never buffered in memory
    and a crash never leaves a partial artifact.
  - v1 limitations (explicit errors): `partitions_from` and `parallel()`
    result values require a local artifact store.
- **`@sink` execution** — previously parsed but inert; sinks now write after
  the parent asset materializes, with `serializer=` override, format
  precedence (kwarg → extension → primary format), per-partition filename
  suffixing, and error isolation (a sink failure never fails the asset;
  outcomes are logged and persisted). Sinks and `@asset(serializer=)` now
  participate in the definition hash — a one-time global cache invalidation
  on upgrade.
- **Error surfacing** — worker failures carry the exception type and a
  barca-frame-filtered traceback all the way to `BarcaError` and the DB.
- **Real retries** — backoff is actually applied (`retry_backoff * attempt`,
  non-blocking), every attempt runs in a fresh worker process, and attempt
  counts are recorded accurately; timeouts are reported as `TimeoutError`
  instead of a worker disconnect.

Planned (carried forward):

- **Reproducible Docker benchmarks** — containerized cross-framework comparisons
  with proper timeouts and resource constraints
  ([#65](https://github.com/ExSidius/barca/issues/65))
- **Backend abstraction** — pluggable DB + storage, enabling Docker and shared
  deployments ([#56](https://github.com/ExSidius/barca/issues/56))
- **APM integration** — Datadog + Sentry for observability
  ([#59](https://github.com/ExSidius/barca/issues/59))
- **Alerting hooks** — Slack webhooks, email notifications
  ([#52](https://github.com/ExSidius/barca/issues/52))

## 0.6.0 (shipped)

Goal: object storage on equal footing across clouds, held to one contract.

- **First-class S3, GCS, and Cloudflare R2** — alongside Azure, the object
  stores are now peers, all held to the same shared-state contract
  (conditional create, cross-machine cache hit, concurrent-writer conflict →
  replay). R2 rides on the S3 backend (`s3://` + an R2 endpoint in
  `storage_options`); see [Remote Storage](/reference/remote-storage/).
- **Backend conformance suite** — the identical pull/push/conflict/stale-token
  assertions run against every backend on every PR, using local emulators
  (MinIO for S3/R2, fake-gcs-server for GCS, Azurite for Azure) — no cloud
  credentials. A stale-token guard makes a too-lenient emulator fail loud
  instead of false-passing.
- **Fixes surfaced by that suite:**
  - Azure concurrent first-push races (`FileExistsError` from adlfs
    create-only) are now classified as conflicts and replayed, not hard errors.
  - GCS conditional overwrite (previously broken — the etag was fed to
    `int(if_generation_match)`); GCS shared state now rides entirely on the
    google-cloud-storage SDK, conditioning on the numeric generation.
  - `barca[gcs]`/`barca[remote]` now declare `google-cloud-storage`, which the
    state push path imports directly (gcsfs does not pull it in).

## 0.5.0 (shipped)

Goal: shared state across machines.

- **Shared remote materialization state** — the metadata DB lives as a blob
  (`{uri}/{env}/state/metadata.db`), pulled at run start and pushed with an
  etag/generation-conditional upload at run end (conflict → pull + replay).
  A run on one machine hits artifacts materialized by another.
- **Content-addressed artifacts** — `{artifacts}/{node}/{run_hash}{ext}` in
  every mode (local included): immutable objects, cross-machine cache hits.
- **barca.toml** — first config file (`[remote]` section, `default_env`),
  env vars override; see [Configuration](/reference/config/).
- **`--env` environments** — dev/staging/prod fully separated state, local
  and remote.

Planned follow-ups: `barca serve` with shared state (currently gated to
`state = "off"`), `barca gc` for content-addressed artifact garbage
collection, partitioned cache checks.

## Future (unscoped)

Not yet assigned to a release:

- Partition filtering on CLI (`--partition` flag)
  ([#57](https://github.com/ExSidius/barca/issues/57))
- File versioning for artifacts
  ([#61](https://github.com/ExSidius/barca/issues/61))
- Notebook integration
- TUI
- Distributed execution (multi-machine)
- Web UI
