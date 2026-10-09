# Example: partitions with fan-in

One asset fans out over three keys; a second asset collects every partition into a list.

```python
from barca import asset, collect, partitions


@asset(partitions={"region": partitions(["emea", "amer", "apac"])})
def sales(region: str) -> dict:
    return {"region": region, "revenue": len(region) * 100}


@asset(inputs={"all_sales": collect(sales)})
def summary(all_sales: list[dict]) -> dict:
    return {"regions": len(all_sales), "total": sum(s["revenue"] for s in all_sales)}
```

```bash
barca plan pipeline.py
barca get summary pipeline.py
barca get summary pipeline.py
```

What to notice:

- `barca plan` shows one `sales` step per region, then `summary` in its own fan-in phase.
- The first `get` runs 4 steps. The second runs 0: every partition and the fan-in are served
  from cache.
- Add `"latam"` to the list in the decorator and run `barca get summary pipeline.py` again: 2
  steps run, `sales` for `latam` and `summary`. The other three regions are served from cache.
  Remove a region and only `summary` runs. The keys are not part of the function's definition,
  so editing the list never re-runs a key that is already cached (`barca docs cache`, "Which
  decorator arguments count").
- Each partition has its own artifact under `.barca/artifacts/`, for example
  `pipeline.py--sales_region_emea/`.

See also: `barca docs partitions`.
