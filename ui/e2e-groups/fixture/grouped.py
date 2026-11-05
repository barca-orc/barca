"""A small pipeline declared in groups, served for real by the grouped e2e suite.

Source data feeds two cross-validation folds and a summary; every asset has a check. The
fold 2 check fails on purpose, so a run of it records a failed last attempt while its model
stays cached: what the grouped page must show as two facts, not one.
"""

from barca import asset, group, task


@asset
def raw_rows():
    return [1, 2, 3]


@asset(inputs={"rows": raw_rows})
def clean_rows(rows):
    return [r for r in rows if r > 0]


@task(inputs={"rows": clean_rows})
def check__clean_rows(rows):
    assert len(rows) == 3


@asset(inputs={"rows": clean_rows})
def fold_01__model(rows):
    return {"features": len(rows)}


@task(inputs={"model": fold_01__model})
def check__fold_01__model(model):
    assert model["features"] == 3


@asset(inputs={"rows": clean_rows})
def fold_02__model(rows):
    return {"features": len(rows) - 1}


@task(inputs={"model": fold_02__model})
def check__fold_02__model(model):
    raise AssertionError(f"feature count mismatch: expected 3, got {model['features']}")


@asset(inputs={"a": fold_01__model, "b": fold_02__model})
def summary(a, b):
    return {"folds": 2}


@task(inputs={"s": summary})
def publish(s):
    print("published", s["folds"], "folds", flush=True)


source = group(
    "Source data",
    members=[raw_rows, clean_rows, check__clean_rows],
    output=clean_rows,
    description="Generate and clean the rows.",
)
fold_01 = group("Fold 1", members=[fold_01__model, check__fold_01__model], output=fold_01__model)
fold_02 = group("Fold 2", members=[fold_02__model, check__fold_02__model], output=fold_02__model)
cross_validation = group(
    "Cross-validation",
    members=[fold_01, fold_02, summary],
    output=summary,
    description="Two folds and their summary.",
)
