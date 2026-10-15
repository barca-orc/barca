# Artifact formats

Status: implemented. Specification version: 1.
Scope: choosing CSV artifacts and sinks, reading them and inspecting cached results.
Conformance: `python/tests/test_csv_outputs.py`, `crates/barca-core/tests/grammar_spec.rs`.

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
on failure. Sink failures follow the existing isolated sink failure contract.

Consuming parameter annotations choose pandas (default), Polars DataFrame or
LazyFrame, Arrow Table or DuckDB relation readers, as for Parquet. Remote CSV is
downloaded. Remote lazy CSV readers materialize the download before deleting it,
then expose the requested lazy interface; CSV does not promise ranged remote scans.

Status inspection counts CSV rows and reports columns and sampled cells as strings.
Remote inspection retains the existing whole-object size cap. SQL registers CSV
results using DuckDB's CSV inference with headers. Cache hits preserve CSV format
and downstream steps use the corresponding CSV reader.
