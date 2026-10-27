# Artifact formats

Status: implemented. Specification version: 3 (sinks republished by cache hits, #662; version
2: dataset sinks and failing sinks, 0.30.0).
Scope: choosing CSV artifacts and sinks, dataset sinks, reading them and inspecting cached
results.
Conformance: `python/tests/test_csv_outputs.py`, `python/tests/test_dataset_sinks.py`,
`python/tests/test_sink_republish.py`, `crates/barca-core/src/sink_publications.rs`,
`crates/barca-core/src/sink_args.rs`, `crates/barca-core/tests/grammar_spec.rs`.

Frames continue to default to Parquet. A literal `serializer="csv"` on an asset,
sensor or task selects CSV, and the coordinator preserves that selection through
execution. A sink selects CSV explicitly or by a `.csv` path extension. Artifact
references report `format: "csv"`, a `.csv` path and the serialized byte size.
Sensor content hashes and upload receipts use the actual serialized CSV bytes.

Supported writers are pandas DataFrame, Polars DataFrame/LazyFrame, Arrow Table
and DuckDB relation. CSV includes a header and excludes the pandas index. Lazy
outputs are materialized. CSV uses each library's standard quoting/escaping and
UTF-8 behavior; readers infer column types. CSV does not promise exact dtype,
index, null/empty-string or nested-column preservation. Unsupported values fail;
CSV never silently downgrades to pickle. Staged writes preserve existing bytes
on failure. A sink that cannot be written fails its step with a `SinkError` (version 2;
version 1 isolated sink failures as a stderr warning while the step succeeded).

Consuming parameter annotations choose pandas (default), Polars DataFrame or
LazyFrame, Arrow Table or DuckDB relation readers, as for Parquet. Remote CSV is
downloaded. Remote lazy CSV readers materialize the download before deleting it,
then expose the requested lazy interface; CSV does not promise ranged remote scans.

Status inspection counts CSV rows and reports columns and sampled cells as strings.
Remote inspection retains the existing whole-object size cap. SQL registers CSV
results using DuckDB's CSV inference with headers. Cache hits preserve CSV format
and downstream steps use the corresponding CSV reader.

## Dataset sinks

A `@sink` path ending in `/` is a dataset prefix, written as a Hive-partitioned dataset:

- One directory level `dim=value` per partition dimension of the asset, in declared order,
  then one per `partition_by=` column of the returned table. Values are percent-encoded as
  pyarrow encodes them (`urllib.parse.quote(value, safe="")`); null or empty is
  `__HIVE_DEFAULT_PARTITION__`. Partition columns are not in the files.
- Files are `part-<i>.parquet` (zstd), `part-<i>.csv` (header, no index) or `part-<i>.jsonl`
  (JSON Lines), numbered from 0 in each directory. csv and json write one file per partition;
  `rows_per_file=` caps the rows of a parquet file. A non-table value is one JSON document,
  `part-0.jsonl`. The DuckDB, polars and pyarrow writers produce the same paths.
- `_barca_sink.json` at the prefix records `barca_sink` (1), `asset`, `dims`,
  `partition_by`, `format` and, after a prune, `keys_digest`. A prefix holding files other than
  the marker and dot-files, without a marker, or with a marker naming another asset, is refused.
- A key is staged and swapped in for its directory: an atomic directory exchange on Linux, two
  renames elsewhere, per-object replacement then deletion of the key's old objects on an object
  store. After a run that knew the asset's whole key set (not narrowed by `--partition`), the
  directories of other keys are deleted (`python -m barca._sinks prune`).
- `partition_by=` and `rows_per_file=` must be literals and apply only to a dataset sink;
  violations are usage errors when the file is read.

## Sink publication

Version 3. The coordinator records, per step (a partition key is a step) and per declared sink
path, the run hash whose output the sink holds (`sink_publications`, see
[metadata-schema.md](metadata-schema.md)). It is written with the run's ledger for each sink of a
step that ran and wrote its sinks, and for each sink of a cache hit that was republished or
found current.

- A cache hit whose run hash equals the recorded one writes nothing.
- A cache hit whose run hash differs, or that has no record, is republished: the cached artifact
  is made local (fetched from the artifact store if needed; computed again when it is nowhere,
  which writes the sinks) and `python -m barca._sinks republish` writes each stale sink from it,
  as the worker writes a sink when the step runs. Parquet and CSV artifacts are read as the
  step's annotated return type, pandas when it has none. The step does not run and is reported
  as cached; stderr gets `[barca] sink <path>: republished ...`.
- A sink that fails to be republished fails its step: a `SinkError` failure row, exit 1, a
  `step_failed` envelope; its sinks' records are left as they were.
- With no record (a database from before version 3), the step's latest successful
  `materializations` row with `sinks_json` stands in for it.
- A record of a local sink path counts only on the host that wrote it; a remote URI counts from
  any host. Records are not shared through a server's ledger.
- Version 2 never wrote the sinks of a cache hit: after code A, B, A the sink held B.
