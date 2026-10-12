"""Synthetic binary-classification pipeline for exploring large Barca graphs.

152 nodes: 76 cached assets, each with an independent validation task.
Hierarchy is expressed through names: data__, cv__fold_01__ … cv__fold_05__,
cv__summary__, train__, test__, and release__. Five folds share preparation;
all learned preprocessing is fitted on each fold's training rows only.

Run assets: barca get modeling.py
Run checks: barca run <comma-separated validation task names> modeling.py
Everything uses deterministic synthetic data and the Python standard library.
"""

import math
import random
import statistics

from barca import asset, task


def _generate(spec: dict) -> list[dict]:
    rng = random.Random(spec["seed"])
    rows = []
    for i in range(spec["rows"]):
        age = rng.randint(18, 70)
        income = rng.uniform(20000, 150000)
        visits = rng.randint(0, 30)
        score = (age - 40) / 35 + (income - 65000) / 60000 + (visits - 12) / 10
        probability = 1 / (1 + math.exp(-score))
        rows.append(
            {
                "row_id": i,
                "age": None if i % 31 == 0 else age,
                "income": None if i % 43 == 0 else round(income, 2),
                "visits": visits,
                "converted": int(rng.random() < probability),
            }
        )
    return rows + [dict(row) for row in rows[:3]]  # Deliberate duplicates to clean.


def _profile(rows: list, names: list[str]) -> dict:
    return {
        "rows": len(rows),
        "positive_labels": sum(r["converted"] for r in rows),
        "missing": {name: sum(r[name] is None for r in rows) for name in names},
        "columns": list(rows[0]),
    }


def _fit_scaler(rows: list, names: list[str]) -> dict:
    means = {name: statistics.mean(r[name] for r in rows if r[name] is not None) for name in names}
    scales = {
        name: statistics.pstdev(means[name] if r[name] is None else r[name] for r in rows) or 1.0
        for name in names
    }
    return {
        "features": names,
        "means": means,
        "scales": scales,
        "fit_row_ids": sorted(r["row_id"] for r in rows),
    }


def _transform(rows: list, scaler: dict) -> list[dict]:
    return [
        {
            "row_id": r["row_id"],
            "label": r["converted"],
            "features": [
                ((scaler["means"][name] if r[name] is None else r[name]) - scaler["means"][name])
                / scaler["scales"][name]
                for name in scaler["features"]
            ],
        }
        for r in rows
    ]


def _fit(rows: list, initial: dict) -> dict:
    weights = list(initial["weights"])
    config = initial["configuration"]
    losses = []
    for _ in range(config["iterations"]):
        gradient = [0.0] * len(weights)
        loss = 0.0
        for row in rows:
            x = [1.0, *row["features"]]
            score = max(-30.0, min(30.0, sum(w * value for w, value in zip(weights, x))))
            p = 1 / (1 + math.exp(-score))
            loss -= row["label"] * math.log(max(p, 1e-12)) + (1 - row["label"]) * math.log(
                max(1 - p, 1e-12)
            )
            for j, value in enumerate(x):
                gradient[j] += (p - row["label"]) * value
        weights = [
            w - config["learning_rate"] * (g / len(rows) + (config["l2"] * w if j else 0))
            for j, (w, g) in enumerate(zip(weights, gradient))
        ]
        losses.append(round(loss / len(rows), 6))
    return {
        "weights": weights,
        "losses": losses,
        "training_rows": len(rows),
        "configuration": config,
    }


def _predict(rows: list, model: dict) -> list[dict]:
    result = []
    for row in rows:
        score = max(
            -30.0, min(30.0, sum(w * x for w, x in zip(model["weights"], [1.0, *row["features"]])))
        )
        p = 1 / (1 + math.exp(-score))
        result.append(
            {
                "row_id": row["row_id"],
                "label": row["label"],
                "probability": round(p, 6),
                "prediction": int(p >= 0.5),
            }
        )
    return result


def _metrics(predictions: list) -> dict:
    tp = sum(p["prediction"] == 1 and p["label"] == 1 for p in predictions)
    tn = sum(p["prediction"] == 0 and p["label"] == 0 for p in predictions)
    fp = sum(p["prediction"] == 1 and p["label"] == 0 for p in predictions)
    fn = sum(p["prediction"] == 0 and p["label"] == 1 for p in predictions)
    return {
        "rows": len(predictions),
        "accuracy": (tp + tn) / len(predictions),
        "precision": tp / max(1, tp + fp),
        "recall": tp / max(1, tp + fn),
        "confusion_matrix": {
            "true_positive": tp,
            "true_negative": tn,
            "false_positive": fp,
            "false_negative": fn,
        },
    }


def _check_rows(value: list) -> None:
    assert value and len({r["row_id"] for r in value}) == len(value), (
        "Rows must be nonempty and uniquely identified"
    )
    assert all(r["converted"] in (0, 1) for r in value), "Labels must be binary"


def _check_features(value: list) -> None:
    assert value and all(len(r["features"]) == 3 for r in value), "Expected three model features"
    assert all(math.isfinite(x) for r in value for x in r["features"]), "Features must be finite"


def _check_scaler(value: dict) -> None:
    assert len(value["features"]) == 3 and value["fit_row_ids"]
    assert all(math.isfinite(x) and x > 0 for x in value["scales"].values()), (
        "Scales must be positive and finite"
    )


def _check_fit(value: dict) -> None:
    assert len(value["weights"]) == 4 and all(math.isfinite(w) for w in value["weights"])
    assert len(value["losses"]) == value["configuration"]["iterations"]
    assert value["losses"][-1] < value["losses"][0], "Training must reduce the loss"


def _check_model(value: dict) -> None:
    assert value["algorithm"] == "synthetic_logistic_regression" and len(value["weights"]) == 4
    _check_scaler(value["preprocessing"])


def _check_predictions(value: list) -> None:
    assert value and len({p["row_id"] for p in value}) == len(value)
    assert all(0 <= p["probability"] <= 1 and p["prediction"] in (0, 1) for p in value)


def _check_metrics(value: dict) -> None:
    assert value["rows"] > 0 and sum(value["confusion_matrix"].values()) == value["rows"]
    assert all(0 <= value[name] <= 1 for name in ("accuracy", "precision", "recall"))


def _passed(name: str) -> dict:
    print(f"Validation passed: {name}")
    return {"step": name, "status": "passed"}


@asset()
def data__source_spec():
    return {"rows": 300, "seed": 42, "features": ["age", "income", "visits"], "target": "converted"}


@task(inputs={"value": data__source_spec})
def validate__data__source_spec(value):
    assert value["rows"] >= 100 and len(value["features"]) == 3
    return _passed("data__source_spec")


@asset(inputs={"spec": data__source_spec})
def data__raw_rows(spec):
    return _generate(spec)


@task(inputs={"value": data__raw_rows, "spec": data__source_spec})
def validate__data__raw_rows(value, spec):
    assert len(value) == spec["rows"] + 3
    assert all(set(spec["features"]).issubset(row) for row in value)
    return _passed("data__raw_rows")


@asset(inputs={"rows": data__raw_rows, "spec": data__source_spec})
def data__schema_profile(rows, spec):
    return _profile(rows, spec["features"])


@task(inputs={"value": data__schema_profile})
def validate__data__schema_profile(value):
    assert value["rows"] == 303 and set(value["missing"]) == {"age", "income", "visits"}
    assert value["missing"]["age"] > 0
    return _passed("data__schema_profile")


@asset(inputs={"rows": data__raw_rows})
def data__clean_rows(rows):
    return list({row["row_id"]: row for row in rows}.values())


@task(inputs={"value": data__clean_rows})
def validate__data__clean_rows(value):
    _check_rows(value)
    assert len(value) == 300
    return _passed("data__clean_rows")


@asset(inputs={"spec": data__source_spec})
def data__feature_contract(spec):
    return {
        "features": spec["features"],
        "target": spec["target"],
        "identifier": "row_id",
        "version": 1,
    }


@task(inputs={"value": data__feature_contract})
def validate__data__feature_contract(value):
    assert value["target"] not in value["features"] and value["identifier"] not in value["features"]
    return _passed("data__feature_contract")


@asset(inputs={"rows": data__clean_rows, "spec": data__source_spec})
def data__split_manifest(rows, spec):
    ids = [r["row_id"] for r in rows]
    random.Random(spec["seed"]).shuffle(ids)
    cut = int(len(ids) * 0.8)
    return {"train_ids": sorted(ids[:cut]), "test_ids": sorted(ids[cut:]), "seed": spec["seed"]}


@task(inputs={"value": data__split_manifest})
def validate__data__split_manifest(value):
    assert len(value["train_ids"]) == 240 and len(value["test_ids"]) == 60
    assert not set(value["train_ids"]) & set(value["test_ids"])
    assert len(set(value["train_ids"]) | set(value["test_ids"])) == 300
    return _passed("data__split_manifest")


@asset(inputs={"rows": data__clean_rows, "split": data__split_manifest})
def data__train_rows(rows, split):
    return [r for r in rows if r["row_id"] in set(split["train_ids"])]


@task(inputs={"value": data__train_rows, "split": data__split_manifest})
def validate__data__train_rows(value, split):
    _check_rows(value)
    assert len(value) == 240
    assert {r["row_id"] for r in value} == set(split["train_ids"])
    return _passed("data__train_rows")


@asset(inputs={"rows": data__clean_rows, "split": data__split_manifest})
def data__test_rows(rows, split):
    return [r for r in rows if r["row_id"] in set(split["test_ids"])]


@task(inputs={"value": data__test_rows, "split": data__split_manifest})
def validate__data__test_rows(value, split):
    _check_rows(value)
    assert len(value) == 60
    assert not {r["row_id"] for r in value} & set(split["train_ids"])
    return _passed("data__test_rows")


@asset(inputs={"rows": data__train_rows, "contract": data__feature_contract})
def data__training_profile(rows, contract):
    return _profile(rows, contract["features"])


@task(inputs={"value": data__training_profile})
def validate__data__training_profile(value):
    assert value["rows"] == 240 and 0 < value["positive_labels"] < value["rows"]
    return _passed("data__training_profile")


@asset(inputs={"rows": data__train_rows, "contract": data__feature_contract})
def data__imputation_values(rows, contract):
    return {
        name: statistics.mean(r[name] for r in rows if r[name] is not None)
        for name in contract["features"]
    }


@task(inputs={"value": data__imputation_values})
def validate__data__imputation_values(value):
    assert len(value) == 3 and all(math.isfinite(x) for x in value.values())
    return _passed("data__imputation_values")


@asset(inputs={"rows": data__train_rows})
def data__training_labels(rows):
    return [{"row_id": r["row_id"], "label": r["converted"]} for r in rows]


@task(inputs={"value": data__training_labels})
def validate__data__training_labels(value):
    assert len(value) == 240 and {r["label"] for r in value} == {0, 1}
    return _passed("data__training_labels")


@asset(inputs={"rows": data__test_rows})
def data__test_labels(rows):
    return [{"row_id": r["row_id"], "label": r["converted"]} for r in rows]


@task(inputs={"value": data__test_labels})
def validate__data__test_labels(value):
    assert len(value) == 60 and {r["label"] for r in value} == {0, 1}
    return _passed("data__test_labels")


@asset(inputs={"labels": data__training_labels})
def data__fold_assignments(labels):
    assignments = []
    for label in (0, 1):
        group = [row for row in labels if row["label"] == label]
        random.Random(42 + label).shuffle(group)
        assignments.extend(
            {"row_id": row["row_id"], "fold": index % 5 + 1, "label": label}
            for index, row in enumerate(group)
        )
    return sorted(assignments, key=lambda row: row["row_id"])


@task(inputs={"value": data__fold_assignments})
def validate__data__fold_assignments(value):
    assert len(value) == 240 and len({r["row_id"] for r in value}) == 240
    assert {r["fold"] for r in value} == {1, 2, 3, 4, 5}
    assert all({r["label"] for r in value if r["fold"] == fold} == {0, 1} for fold in range(1, 6))
    return _passed("data__fold_assignments")


@asset()
def data__training_configuration():
    return {"iterations": 60, "learning_rate": 0.15, "l2": 0.01, "threshold": 0.5, "folds": 5}


@task(inputs={"value": data__training_configuration})
def validate__data__training_configuration(value):
    assert value["iterations"] > 0 and 0 < value["learning_rate"] < 1
    assert value["folds"] == 5 and value["threshold"] == 0.5
    return _passed("data__training_configuration")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_01__train_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] != 1}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_01__train_rows, "assignments": data__fold_assignments})
def validate__cv__fold_01__train_rows(value, assignments):
    _check_rows(value)
    assert 185 <= len(value) <= 195
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in assignments if r["fold"] == 1}
    return _passed("cv__fold_01__train_rows")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_01__validation_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] == 1}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_01__validation_rows, "training": cv__fold_01__train_rows})
def validate__cv__fold_01__validation_rows(value, training):
    _check_rows(value)
    assert 45 <= len(value) <= 55
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in training}
    assert len(value) + len(training) == 240
    return _passed("cv__fold_01__validation_rows")


@asset(inputs={"rows": cv__fold_01__train_rows, "contract": data__feature_contract})
def cv__fold_01__scaler(rows, contract):
    return _fit_scaler(rows, contract["features"])


@task(inputs={"value": cv__fold_01__scaler, "held_out": cv__fold_01__validation_rows})
def validate__cv__fold_01__scaler(value, held_out):
    _check_scaler(value)
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in held_out}
    return _passed("cv__fold_01__scaler")


@asset(inputs={"rows": cv__fold_01__train_rows, "scaler": cv__fold_01__scaler})
def cv__fold_01__train_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_01__train_features, "scaler": cv__fold_01__scaler})
def validate__cv__fold_01__train_features(value, scaler):
    _check_features(value)
    assert {r["row_id"] for r in value} == set(scaler["fit_row_ids"])
    return _passed("cv__fold_01__train_features")


@asset(inputs={"rows": cv__fold_01__validation_rows, "scaler": cv__fold_01__scaler})
def cv__fold_01__validation_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_01__validation_features, "scaler": cv__fold_01__scaler})
def validate__cv__fold_01__validation_features(value, scaler):
    _check_features(value)
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("cv__fold_01__validation_features")


@asset(inputs={"contract": data__feature_contract, "configuration": data__training_configuration})
def cv__fold_01__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": cv__fold_01__initial_weights})
def validate__cv__fold_01__initial_weights(value):
    assert value["weights"] == [0.0] * 4
    return _passed("cv__fold_01__initial_weights")


@asset(inputs={"rows": cv__fold_01__train_features, "initial": cv__fold_01__initial_weights})
def cv__fold_01__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": cv__fold_01__fitted_weights})
def validate__cv__fold_01__fitted_weights(value):
    _check_fit(value)
    return _passed("cv__fold_01__fitted_weights")


@asset(inputs={"fit": cv__fold_01__fitted_weights, "scaler": cv__fold_01__scaler})
def cv__fold_01__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
    }


@task(inputs={"value": cv__fold_01__trained_model})
def validate__cv__fold_01__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == len(value["preprocessing"]["fit_row_ids"])
    return _passed("cv__fold_01__trained_model")


@asset(inputs={"rows": cv__fold_01__validation_features, "model": cv__fold_01__trained_model})
def cv__fold_01__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": cv__fold_01__predictions, "held_out": cv__fold_01__validation_rows})
def validate__cv__fold_01__predictions(value, held_out):
    _check_predictions(value)
    assert {p["row_id"] for p in value} == {r["row_id"] for r in held_out}
    return _passed("cv__fold_01__predictions")


@asset(inputs={"predictions": cv__fold_01__predictions})
def cv__fold_01__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": cv__fold_01__metrics})
def validate__cv__fold_01__metrics(value):
    _check_metrics(value)
    assert value["accuracy"] >= 0.5, "Model must beat the demo baseline"
    return _passed("cv__fold_01__metrics")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_02__train_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] != 2}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_02__train_rows, "assignments": data__fold_assignments})
def validate__cv__fold_02__train_rows(value, assignments):
    _check_rows(value)
    assert 185 <= len(value) <= 195
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in assignments if r["fold"] == 2}
    return _passed("cv__fold_02__train_rows")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_02__validation_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] == 2}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_02__validation_rows, "training": cv__fold_02__train_rows})
def validate__cv__fold_02__validation_rows(value, training):
    _check_rows(value)
    assert 45 <= len(value) <= 55
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in training}
    assert len(value) + len(training) == 240
    return _passed("cv__fold_02__validation_rows")


@asset(inputs={"rows": cv__fold_02__train_rows, "contract": data__feature_contract})
def cv__fold_02__scaler(rows, contract):
    return _fit_scaler(rows, contract["features"])


@task(inputs={"value": cv__fold_02__scaler, "held_out": cv__fold_02__validation_rows})
def validate__cv__fold_02__scaler(value, held_out):
    _check_scaler(value)
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in held_out}
    return _passed("cv__fold_02__scaler")


@asset(inputs={"rows": cv__fold_02__train_rows, "scaler": cv__fold_02__scaler})
def cv__fold_02__train_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_02__train_features, "scaler": cv__fold_02__scaler})
def validate__cv__fold_02__train_features(value, scaler):
    _check_features(value)
    assert {r["row_id"] for r in value} == set(scaler["fit_row_ids"])
    return _passed("cv__fold_02__train_features")


@asset(inputs={"rows": cv__fold_02__validation_rows, "scaler": cv__fold_02__scaler})
def cv__fold_02__validation_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_02__validation_features, "scaler": cv__fold_02__scaler})
def validate__cv__fold_02__validation_features(value, scaler):
    _check_features(value)
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("cv__fold_02__validation_features")


@asset(inputs={"contract": data__feature_contract, "configuration": data__training_configuration})
def cv__fold_02__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": cv__fold_02__initial_weights})
def validate__cv__fold_02__initial_weights(value):
    assert value["weights"] == [0.0] * 4
    return _passed("cv__fold_02__initial_weights")


@asset(inputs={"rows": cv__fold_02__train_features, "initial": cv__fold_02__initial_weights})
def cv__fold_02__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": cv__fold_02__fitted_weights})
def validate__cv__fold_02__fitted_weights(value):
    _check_fit(value)
    return _passed("cv__fold_02__fitted_weights")


@asset(inputs={"fit": cv__fold_02__fitted_weights, "scaler": cv__fold_02__scaler})
def cv__fold_02__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
    }


@task(inputs={"value": cv__fold_02__trained_model})
def validate__cv__fold_02__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == len(value["preprocessing"]["fit_row_ids"])
    return _passed("cv__fold_02__trained_model")


@asset(inputs={"rows": cv__fold_02__validation_features, "model": cv__fold_02__trained_model})
def cv__fold_02__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": cv__fold_02__predictions, "held_out": cv__fold_02__validation_rows})
def validate__cv__fold_02__predictions(value, held_out):
    _check_predictions(value)
    assert {p["row_id"] for p in value} == {r["row_id"] for r in held_out}
    return _passed("cv__fold_02__predictions")


@asset(inputs={"predictions": cv__fold_02__predictions})
def cv__fold_02__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": cv__fold_02__metrics})
def validate__cv__fold_02__metrics(value):
    _check_metrics(value)
    assert value["accuracy"] >= 0.5, "Model must beat the demo baseline"
    return _passed("cv__fold_02__metrics")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_03__train_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] != 3}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_03__train_rows, "assignments": data__fold_assignments})
def validate__cv__fold_03__train_rows(value, assignments):
    _check_rows(value)
    assert 185 <= len(value) <= 195
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in assignments if r["fold"] == 3}
    return _passed("cv__fold_03__train_rows")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_03__validation_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] == 3}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_03__validation_rows, "training": cv__fold_03__train_rows})
def validate__cv__fold_03__validation_rows(value, training):
    _check_rows(value)
    assert 45 <= len(value) <= 55
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in training}
    assert len(value) + len(training) == 240
    return _passed("cv__fold_03__validation_rows")


@asset(inputs={"rows": cv__fold_03__train_rows, "contract": data__feature_contract})
def cv__fold_03__scaler(rows, contract):
    return _fit_scaler(rows, contract["features"])


@task(inputs={"value": cv__fold_03__scaler, "held_out": cv__fold_03__validation_rows})
def validate__cv__fold_03__scaler(value, held_out):
    _check_scaler(value)
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in held_out}
    return _passed("cv__fold_03__scaler")


@asset(inputs={"rows": cv__fold_03__train_rows, "scaler": cv__fold_03__scaler})
def cv__fold_03__train_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_03__train_features, "scaler": cv__fold_03__scaler})
def validate__cv__fold_03__train_features(value, scaler):
    _check_features(value)
    assert {r["row_id"] for r in value} == set(scaler["fit_row_ids"])
    return _passed("cv__fold_03__train_features")


@asset(inputs={"rows": cv__fold_03__validation_rows, "scaler": cv__fold_03__scaler})
def cv__fold_03__validation_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_03__validation_features, "scaler": cv__fold_03__scaler})
def validate__cv__fold_03__validation_features(value, scaler):
    _check_features(value)
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("cv__fold_03__validation_features")


@asset(inputs={"contract": data__feature_contract, "configuration": data__training_configuration})
def cv__fold_03__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": cv__fold_03__initial_weights})
def validate__cv__fold_03__initial_weights(value):
    assert value["weights"] == [0.0] * 4
    return _passed("cv__fold_03__initial_weights")


@asset(inputs={"rows": cv__fold_03__train_features, "initial": cv__fold_03__initial_weights})
def cv__fold_03__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": cv__fold_03__fitted_weights})
def validate__cv__fold_03__fitted_weights(value):
    _check_fit(value)
    return _passed("cv__fold_03__fitted_weights")


@asset(inputs={"fit": cv__fold_03__fitted_weights, "scaler": cv__fold_03__scaler})
def cv__fold_03__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
    }


@task(inputs={"value": cv__fold_03__trained_model})
def validate__cv__fold_03__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == len(value["preprocessing"]["fit_row_ids"])
    return _passed("cv__fold_03__trained_model")


@asset(inputs={"rows": cv__fold_03__validation_features, "model": cv__fold_03__trained_model})
def cv__fold_03__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": cv__fold_03__predictions, "held_out": cv__fold_03__validation_rows})
def validate__cv__fold_03__predictions(value, held_out):
    _check_predictions(value)
    assert {p["row_id"] for p in value} == {r["row_id"] for r in held_out}
    return _passed("cv__fold_03__predictions")


@asset(inputs={"predictions": cv__fold_03__predictions})
def cv__fold_03__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": cv__fold_03__metrics})
def validate__cv__fold_03__metrics(value):
    _check_metrics(value)
    assert value["accuracy"] >= 0.5, "Model must beat the demo baseline"
    return _passed("cv__fold_03__metrics")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_04__train_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] != 4}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_04__train_rows, "assignments": data__fold_assignments})
def validate__cv__fold_04__train_rows(value, assignments):
    _check_rows(value)
    assert 185 <= len(value) <= 195
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in assignments if r["fold"] == 4}
    return _passed("cv__fold_04__train_rows")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_04__validation_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] == 4}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_04__validation_rows, "training": cv__fold_04__train_rows})
def validate__cv__fold_04__validation_rows(value, training):
    _check_rows(value)
    assert 45 <= len(value) <= 55
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in training}
    assert len(value) + len(training) == 240
    return _passed("cv__fold_04__validation_rows")


@asset(inputs={"rows": cv__fold_04__train_rows, "contract": data__feature_contract})
def cv__fold_04__scaler(rows, contract):
    return _fit_scaler(rows, contract["features"])


@task(inputs={"value": cv__fold_04__scaler, "held_out": cv__fold_04__validation_rows})
def validate__cv__fold_04__scaler(value, held_out):
    _check_scaler(value)
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in held_out}
    return _passed("cv__fold_04__scaler")


@asset(inputs={"rows": cv__fold_04__train_rows, "scaler": cv__fold_04__scaler})
def cv__fold_04__train_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_04__train_features, "scaler": cv__fold_04__scaler})
def validate__cv__fold_04__train_features(value, scaler):
    _check_features(value)
    assert {r["row_id"] for r in value} == set(scaler["fit_row_ids"])
    return _passed("cv__fold_04__train_features")


@asset(inputs={"rows": cv__fold_04__validation_rows, "scaler": cv__fold_04__scaler})
def cv__fold_04__validation_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_04__validation_features, "scaler": cv__fold_04__scaler})
def validate__cv__fold_04__validation_features(value, scaler):
    _check_features(value)
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("cv__fold_04__validation_features")


@asset(inputs={"contract": data__feature_contract, "configuration": data__training_configuration})
def cv__fold_04__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": cv__fold_04__initial_weights})
def validate__cv__fold_04__initial_weights(value):
    assert value["weights"] == [0.0] * 4
    return _passed("cv__fold_04__initial_weights")


@asset(inputs={"rows": cv__fold_04__train_features, "initial": cv__fold_04__initial_weights})
def cv__fold_04__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": cv__fold_04__fitted_weights})
def validate__cv__fold_04__fitted_weights(value):
    _check_fit(value)
    return _passed("cv__fold_04__fitted_weights")


@asset(inputs={"fit": cv__fold_04__fitted_weights, "scaler": cv__fold_04__scaler})
def cv__fold_04__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
    }


@task(inputs={"value": cv__fold_04__trained_model})
def validate__cv__fold_04__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == len(value["preprocessing"]["fit_row_ids"])
    return _passed("cv__fold_04__trained_model")


@asset(inputs={"rows": cv__fold_04__validation_features, "model": cv__fold_04__trained_model})
def cv__fold_04__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": cv__fold_04__predictions, "held_out": cv__fold_04__validation_rows})
def validate__cv__fold_04__predictions(value, held_out):
    _check_predictions(value)
    assert {p["row_id"] for p in value} == {r["row_id"] for r in held_out}
    return _passed("cv__fold_04__predictions")


@asset(inputs={"predictions": cv__fold_04__predictions})
def cv__fold_04__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": cv__fold_04__metrics})
def validate__cv__fold_04__metrics(value):
    _check_metrics(value)
    assert value["accuracy"] >= 0.5, "Model must beat the demo baseline"
    return _passed("cv__fold_04__metrics")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_05__train_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] != 5}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_05__train_rows, "assignments": data__fold_assignments})
def validate__cv__fold_05__train_rows(value, assignments):
    _check_rows(value)
    assert 185 <= len(value) <= 195
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in assignments if r["fold"] == 5}
    return _passed("cv__fold_05__train_rows")


@asset(inputs={"rows": data__train_rows, "assignments": data__fold_assignments})
def cv__fold_05__validation_rows(rows, assignments):
    ids = {r["row_id"] for r in assignments if r["fold"] == 5}
    return [r for r in rows if r["row_id"] in ids]


@task(inputs={"value": cv__fold_05__validation_rows, "training": cv__fold_05__train_rows})
def validate__cv__fold_05__validation_rows(value, training):
    _check_rows(value)
    assert 45 <= len(value) <= 55
    assert not {r["row_id"] for r in value} & {r["row_id"] for r in training}
    assert len(value) + len(training) == 240
    return _passed("cv__fold_05__validation_rows")


@asset(inputs={"rows": cv__fold_05__train_rows, "contract": data__feature_contract})
def cv__fold_05__scaler(rows, contract):
    return _fit_scaler(rows, contract["features"])


@task(inputs={"value": cv__fold_05__scaler, "held_out": cv__fold_05__validation_rows})
def validate__cv__fold_05__scaler(value, held_out):
    _check_scaler(value)
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in held_out}
    return _passed("cv__fold_05__scaler")


@asset(inputs={"rows": cv__fold_05__train_rows, "scaler": cv__fold_05__scaler})
def cv__fold_05__train_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_05__train_features, "scaler": cv__fold_05__scaler})
def validate__cv__fold_05__train_features(value, scaler):
    _check_features(value)
    assert {r["row_id"] for r in value} == set(scaler["fit_row_ids"])
    return _passed("cv__fold_05__train_features")


@asset(inputs={"rows": cv__fold_05__validation_rows, "scaler": cv__fold_05__scaler})
def cv__fold_05__validation_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": cv__fold_05__validation_features, "scaler": cv__fold_05__scaler})
def validate__cv__fold_05__validation_features(value, scaler):
    _check_features(value)
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("cv__fold_05__validation_features")


@asset(inputs={"contract": data__feature_contract, "configuration": data__training_configuration})
def cv__fold_05__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": cv__fold_05__initial_weights})
def validate__cv__fold_05__initial_weights(value):
    assert value["weights"] == [0.0] * 4
    return _passed("cv__fold_05__initial_weights")


@asset(inputs={"rows": cv__fold_05__train_features, "initial": cv__fold_05__initial_weights})
def cv__fold_05__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": cv__fold_05__fitted_weights})
def validate__cv__fold_05__fitted_weights(value):
    _check_fit(value)
    return _passed("cv__fold_05__fitted_weights")


@asset(inputs={"fit": cv__fold_05__fitted_weights, "scaler": cv__fold_05__scaler})
def cv__fold_05__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
    }


@task(inputs={"value": cv__fold_05__trained_model})
def validate__cv__fold_05__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == len(value["preprocessing"]["fit_row_ids"])
    return _passed("cv__fold_05__trained_model")


@asset(inputs={"rows": cv__fold_05__validation_features, "model": cv__fold_05__trained_model})
def cv__fold_05__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": cv__fold_05__predictions, "held_out": cv__fold_05__validation_rows})
def validate__cv__fold_05__predictions(value, held_out):
    _check_predictions(value)
    assert {p["row_id"] for p in value} == {r["row_id"] for r in held_out}
    return _passed("cv__fold_05__predictions")


@asset(inputs={"predictions": cv__fold_05__predictions})
def cv__fold_05__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": cv__fold_05__metrics})
def validate__cv__fold_05__metrics(value):
    _check_metrics(value)
    assert value["accuracy"] >= 0.5, "Model must beat the demo baseline"
    return _passed("cv__fold_05__metrics")


@asset(
    inputs={
        "fold_01": cv__fold_01__metrics,
        "fold_02": cv__fold_02__metrics,
        "fold_03": cv__fold_03__metrics,
        "fold_04": cv__fold_04__metrics,
        "fold_05": cv__fold_05__metrics,
    }
)
def cv__summary__fold_metrics(fold_01, fold_02, fold_03, fold_04, fold_05):
    return [
        {"fold": 1, **fold_01},
        {"fold": 2, **fold_02},
        {"fold": 3, **fold_03},
        {"fold": 4, **fold_04},
        {"fold": 5, **fold_05},
    ]


@task(inputs={"value": cv__summary__fold_metrics})
def validate__cv__summary__fold_metrics(value):
    assert len(value) == 5 and sum(v["rows"] for v in value) == 240
    for metric in value:
        _check_metrics(metric)
    return _passed("cv__summary__fold_metrics")


@asset(inputs={"metrics": cv__summary__fold_metrics})
def cv__summary__report(metrics):
    return {
        "folds": len(metrics),
        "mean_accuracy": statistics.mean(m["accuracy"] for m in metrics),
        "std_accuracy": statistics.pstdev(m["accuracy"] for m in metrics),
        "min_accuracy": min(m["accuracy"] for m in metrics),
        "validation_rows": sum(m["rows"] for m in metrics),
    }


@task(inputs={"value": cv__summary__report})
def validate__cv__summary__report(value):
    assert value["folds"] == 5 and value["validation_rows"] == 240
    assert 0.5 <= value["mean_accuracy"] <= 1 and value["std_accuracy"] >= 0
    return _passed("cv__summary__report")


@asset(inputs={"configuration": data__training_configuration, "report": cv__summary__report})
def train__validated_configuration(configuration, report):
    assert report["mean_accuracy"] >= 0.5
    return {**configuration, "cv_mean_accuracy": report["mean_accuracy"]}


@task(inputs={"value": train__validated_configuration})
def validate__train__validated_configuration(value):
    assert value["cv_mean_accuracy"] >= 0.5 and value["iterations"] == 60
    return _passed("train__validated_configuration")


@asset(
    inputs={
        "rows": data__train_rows,
        "contract": data__feature_contract,
        "imputation": data__imputation_values,
    }
)
def train__final_scaler(rows, contract, imputation):
    scaler = _fit_scaler(rows, contract["features"])
    assert scaler["means"] == imputation
    return scaler


@task(inputs={"value": train__final_scaler, "test_rows": data__test_rows})
def validate__train__final_scaler(value, test_rows):
    _check_scaler(value)
    assert len(value["fit_row_ids"]) == 240
    assert not set(value["fit_row_ids"]) & {r["row_id"] for r in test_rows}
    return _passed("train__final_scaler")


@asset(inputs={"rows": data__train_rows, "scaler": train__final_scaler})
def train__final_features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": train__final_features})
def validate__train__final_features(value):
    _check_features(value)
    assert len(value) == 240
    return _passed("train__final_features")


@asset(inputs={"contract": data__feature_contract, "configuration": train__validated_configuration})
def train__initial_weights(contract, configuration):
    return {"weights": [0.0] * (len(contract["features"]) + 1), "configuration": configuration}


@task(inputs={"value": train__initial_weights})
def validate__train__initial_weights(value):
    assert value["weights"] == [0.0] * 4 and value["configuration"]["cv_mean_accuracy"] >= 0.5
    return _passed("train__initial_weights")


@asset(inputs={"rows": train__final_features, "initial": train__initial_weights})
def train__fitted_weights(rows, initial):
    return _fit(rows, initial)


@task(inputs={"value": train__fitted_weights})
def validate__train__fitted_weights(value):
    _check_fit(value)
    assert value["training_rows"] == 240
    return _passed("train__fitted_weights")


@asset(inputs={"fit": train__fitted_weights, "scaler": train__final_scaler})
def train__trained_model(fit, scaler):
    return {
        "algorithm": "synthetic_logistic_regression",
        "weights": fit["weights"],
        "preprocessing": scaler,
        "training_rows": fit["training_rows"],
        "configuration": fit["configuration"],
        "version": "demo-v1",
    }


@task(inputs={"value": train__trained_model})
def validate__train__trained_model(value):
    _check_model(value)
    assert value["training_rows"] == 240 and value["version"] == "demo-v1"
    return _passed("train__trained_model")


@asset(inputs={"rows": data__test_rows, "scaler": train__final_scaler})
def test__features(rows, scaler):
    return _transform(rows, scaler)


@task(inputs={"value": test__features, "scaler": train__final_scaler})
def validate__test__features(value, scaler):
    _check_features(value)
    assert len(value) == 60
    assert not {r["row_id"] for r in value} & set(scaler["fit_row_ids"])
    return _passed("test__features")


@asset(inputs={"rows": test__features, "model": train__trained_model})
def test__predictions(rows, model):
    return _predict(rows, model)


@task(inputs={"value": test__predictions, "labels": data__test_labels})
def validate__test__predictions(value, labels):
    _check_predictions(value)
    assert len(value) == 60
    assert {p["row_id"]: p["label"] for p in value} == {r["row_id"]: r["label"] for r in labels}
    return _passed("test__predictions")


@asset(inputs={"predictions": test__predictions})
def test__metrics(predictions):
    return _metrics(predictions)


@task(inputs={"value": test__metrics})
def validate__test__metrics(value):
    _check_metrics(value)
    assert value["rows"] == 60 and value["accuracy"] >= 0.5
    return _passed("test__metrics")


@asset(
    inputs={
        "model": train__trained_model,
        "test": test__metrics,
        "cv": cv__summary__report,
        "profile": data__training_profile,
    }
)
def release__model_card(model, test, cv, profile):
    return {
        "name": "Synthetic conversion model",
        "version": model["version"],
        "algorithm": model["algorithm"],
        "training_rows": profile["rows"],
        "test_rows": test["rows"],
        "features": model["preprocessing"]["features"],
        "test_accuracy": test["accuracy"],
        "cross_validation": cv,
        "usage": "Synthetic demo only; intended for UI hierarchy experiments",
    }


@task(inputs={"value": release__model_card})
def validate__release__model_card(value):
    assert value["training_rows"] == 240 and value["test_rows"] == 60
    assert value["cross_validation"]["folds"] == 5 and value["test_accuracy"] >= 0.5
    return _passed("release__model_card")
