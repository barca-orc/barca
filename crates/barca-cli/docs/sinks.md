# Sinks

`@sink` writes an asset's output to an extra location whenever the asset materializes, for
example to export a file for another system. It is stacked on top of `@asset`.

```python
import pandas as pd
from barca import asset, sink


@asset()
@sink("./exports/orders.parquet")
@sink("s3://my-bucket/exports/orders.parquet")
def orders() -> pd.DataFrame:
    return pd.DataFrame({"id": [1, 2], "amount": [9.5, 20.0]})


@asset()
@sink("./exports/summary.json")
def summary() -> dict:
    return {"orders": 2}
```

- Paths are local or any fsspec URI (`abfss://`, `s3://`, `gs://`). Remote schemes need the
  matching extra (`pip install "barca[s3]"`, `[azure]`, `[gcs]`, or `[remote]`).
- The format comes from `serializer=` (`json`, `pickle`, `parquet`), else the path extension
  (`.json`, `.pkl`/`.pickle`, `.parquet`), else the parent asset's artifact format.
- A parquet sink needs a DataFrame, Arrow table or DuckDB relation. Anything else is a sink
  failure: barca never writes pickle bytes under a `.parquet` name.
- Several `@sink` decorators may be stacked on one asset.
- `@sink` takes the path by position and `serializer=` by keyword, nothing else.
  `@sink(path="out.json")`, `@sink("out.txt", "json")` and an unknown keyword are errors when
  the file is read, exit 2 (`barca docs assets`, "Accepted arguments"). Up to 0.18.1 the first
  declared no sink and the second ignored the format.
- Writes are staged and finalized atomically, so a crash never leaves a partial file.
- A sink path is yours: barca writes the file there and changes nothing else. If the path is a
  symlink, the file it points to is written and the link stays. If a directory is at the path
  (or the link leads to one), the sink fails and the directory is left as it is.
- Sinks are leaf nodes: no other node may take a sink as an input.
- A failing sink does **not** fail the parent asset. It is reported on stderr as
  `[barca] SINK FAILED: ...`; check stderr in automation.
- For partitioned assets each partition writes its own file with the key inserted before the
  extension: `out.parquet` becomes `out_region_emea.parquet`.

See also: `barca docs types`, `barca docs partitions`.
