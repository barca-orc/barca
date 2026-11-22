# Artifact formats

Status: implemented. Specification version: 6 (input readers retain their annotated type through optional wrappers
and remote-only batched reads, #1103 and #814; version 5: a file sink in its artifact's format holds the
artifact's bytes, and sink time is reported beside the step's, #1005 and #1004; version 4: the
artifact file extension table pinned, #800; version 3: sinks republished by cache hits, #662;
version 2: dataset sinks and failing sinks, 0.30.0).
Scope: choosing artifact readers, CSV artifacts and sinks, dataset sinks, reading them and inspecting cached
results.
Conformance: `python/tests/test_input_type_correctness.py`, `python/tests/test_artifact_extensions.py`, `crates/barca-model/src/artifact_format.rs`
(`format_of_agrees_with_the_spec_table`), `python/tests/test_csv_outputs.py`, `python/tests/test_dataset_sinks.py`,
`python/tests/test_sink_republish.py`, `python/tests/test_sink_copies_artifact.py`,
`crates/barca-db/src/sink_publications.rs`,
`crates/barca-python/src/sink_args.rs`, `crates/barca-service/tests/grammar_spec.rs`.

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

## Input reader selection

Optional annotations (`T | None`, `None | T`, `Optional[T]`, `Union[T, None]`, including
qualified `typing` forms and quoted annotations) retain the reader of `T`, including inside
collect lists and batched dictionaries. A union with more than one non-None type does not
select a reader. `pa.Table` and `pyarrow.Table` both select Arrow Table.

Remote-only parquet inputs use in-place range reads only for DuckDB relations and Polars
LazyFrames, including batched calls. Those readers use Barca's filesystem and configured
credentials. Dataset inputs are fetched into the local artifact directory first, and the
consumer receives a Dataset over local files. Laziness on local files does not imply support
for in-place remote reads. No remote path may substitute a different annotated type.

## Artifact file extensions

An artifact file's extension names its format. Barca reads a format from an extension with this
table, in the worker (`barca._artifacts.FORMAT_OF_EXTENSION`) and in `barca sql`
(`artifact_format::format_of`), and writes each format with the extension marked *written*. Any other
extension is not an artifact format: the worker reads it as JSON, `barca sql` reports it as
`unknown`.

| Extension | Format | Written |
| --- | --- | --- |
| `.json` | json | yes |
| `.pkl` | pickle | yes |
| `.pickle` | pickle | no |
| `.parquet` | parquet | yes |
| `.csv` | csv | yes |

A dataset sink writes JSON as `.jsonl` files, which are sink files, not artifacts.

## File sinks

Version 5. A file sink (a `@sink` path not ending in `/`) is written in its format: its
`serializer=`, else the format its path's extension names in the table above, else the artifact's.

- **In the artifact's format, a file sink holds the artifact's bytes.** The worker copies the bytes
  it wrote for the artifact to the sink's path; it does not encode the step's result again. A
  returned DuckDB relation or Polars LazyFrame therefore runs once, and the sink is byte-identical
  to the artifact even when encoding the result twice would not be (an aggregate's row order). This
  holds for every format (json, pickle, parquet, csv), for each key of a partitioned asset (its
  `out_<key>.<ext>` copies that key's artifact), for a key of a batched call, and through a sink
  path that is a symlink (the file it points to is written, the link stays).
- The bytes copied are this step's: the upload snapshot pinned before the reusable artifact path
  was published ([artifact-upload-receipts.md](artifact-upload-receipts.md)), else a private hard
  link to the staged file taken at the same moment and removed before the step is reported. Another
  run that replaces the artifact path with the same run hash meanwhile cannot change what the sink
  receives. An artifact the worker wrote to a remote path has neither, and its sinks are encoded.
- **In another format, a file sink encodes the result**, as before version 5: a `.csv` sink of a
  parquet artifact, a `.parquet` sink of an asset whose `serializer="pickle"`. A returned relation
  then runs again for that sink. A value that fell back to pickle cannot be a `.parquet` sink: the
  sink fails its step. A dataset sink is always written from the result (below).
- A sink is still written inside its step, after the artifact and before the step is reported; a
  sink that fails still fails its step with a `SinkError` (phase `sink`).
- Republishing a cache hit (below) copies the cached artifact's bytes to each file sink in its
  format without reading the value; the value is read only for a sink in another format or a
  dataset sink. A republished sink then holds the bytes it held when the step ran.

Sink time is reported beside the step's own time, never inside it: the step's `elapsed_seconds`
(and the worker's `wall_seconds`) are taken before its sinks are written. The worker reports
`sink_seconds` (all of the step's sinks) and `seconds` on each sink's outcome
([worker-protocol.md](worker-protocol.md)); the coordinator records `materializations.sink_seconds`
([metadata-schema.md](metadata-schema.md)) and the per-sink outcomes in `sinks_json`, and reports
`steps[].sink_seconds` in `barca get`/`barca run --json` (summed over a node's executed keys) and
`barca history RUN_ID --json`. A step's `barca.step` span covers its sinks and carries
`barca.sink_seconds`.

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
  as the worker writes a sink when the step runs: a file sink in the artifact's format is a copy
  of its bytes (version 5). For any other sink, parquet and CSV artifacts are read as the step's
  annotated return type, pandas when it has none. The step does not run and is reported
  as cached; stderr gets `[barca] sink <path>: republished ...`.
- A sink that fails to be republished fails its step: a `SinkError` failure row, exit 1, a
  `step_failed` envelope; its sinks' records are left as they were.
- With no record (a database from before version 3), the step's latest successful
  `materializations` row with `sinks_json` stands in for it.
- A record of a local sink path counts only on the host that wrote it; a remote URI counts from
  any host. Records are not shared through a server's ledger.
- Version 2 never wrote the sinks of a cache hit: after code A, B, A the sink held B.
