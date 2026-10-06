"""Iris ML pipeline — load, split, train, evaluate, deploy, notify.

Demonstrates a realistic *mixed* pipeline: the data stages (load → split →
train → evaluate) are cacheable ``@asset`` nodes, while the workflow-management
tail (deploy the model, notify the team) are ``@task`` nodes — they always
re-run, are never cached, and consume upstream outputs.

- ``@sink`` on evaluation writes a JSON report to ``tmp/iris_eval.json``.
- ``deploy_model`` is a ``@task`` consuming the ``evaluation`` *asset* (as ``eval_result``).
- ``notify_team`` is a ``@task`` consuming the ``deploy_model`` *task* — a
  nested task. Targeting it (``barca run notify_team ...``) scopes the run to
  exactly this subtree, pulling the assets + deploy behind it.

Try it::

    barca run notify_team iris_project/assets.py
    barca run notify_team --refresh trained_model iris_project/assets.py
"""

from barca import asset, sink, task


@asset()
def raw_data() -> dict:
    """Load the iris dataset."""
    import time

    from sklearn.datasets import load_iris

    print("loading iris dataset…")
    iris = load_iris()
    n = len(iris.data)
    for i in range(0, n, 30):
        print(f"  read {min(i + 30, n)}/{n} samples")
        time.sleep(0.4)
    print(f"done · {n} samples, {len(iris.feature_names)} features")
    return {
        "features": iris.data.tolist(),
        "targets": iris.target.tolist(),
        "feature_names": iris.feature_names,
        "target_names": iris.target_names.tolist(),
    }


@asset(inputs={"raw_data": raw_data})
def train_test_split(raw_data: dict) -> dict:
    """Split into 80/20 train/test."""
    from sklearn.model_selection import train_test_split as sklearn_split

    X_train, X_test, y_train, y_test = sklearn_split(
        raw_data["features"],
        raw_data["targets"],
        test_size=0.2,
        random_state=42,
    )
    return {
        "X_train": X_train,
        "X_test": X_test,
        "y_train": y_train,
        "y_test": y_test,
        "feature_names": raw_data["feature_names"],
        "target_names": raw_data["target_names"],
    }


@asset(inputs={"split": train_test_split})
def trained_model(split: dict) -> dict:
    """Train a random forest classifier."""
    from sklearn.ensemble import RandomForestClassifier

    clf = RandomForestClassifier(n_estimators=50, random_state=42)
    clf.fit(split["X_train"], split["y_train"])

    # Serialize the model params (can't pickle into JSON, so store predictions + accuracy)
    train_accuracy = clf.score(split["X_train"], split["y_train"])
    predictions = clf.predict(split["X_test"]).tolist()

    return {
        "predictions": predictions,
        "train_accuracy": round(train_accuracy, 4),
        "n_estimators": 50,
        "feature_importances": [round(f, 4) for f in clf.feature_importances_.tolist()],
    }


@asset(inputs={"model": trained_model, "split": train_test_split})
@sink("tmp/iris_eval.json", serializer="json")
def evaluation(model: dict, split: dict) -> dict:
    """Evaluate the trained model on test data.

    The ``@sink`` decorator writes this asset's output to
    ``tmp/iris_eval.json`` every time it materialises successfully.
    """
    from sklearn.metrics import accuracy_score, classification_report

    y_test = split["y_test"]
    predictions = model["predictions"]
    accuracy = accuracy_score(y_test, predictions)
    report = classification_report(
        y_test, predictions, target_names=split["target_names"], output_dict=True
    )

    return {
        "test_accuracy": round(accuracy, 4),
        "train_accuracy": model["train_accuracy"],
        "feature_importances": dict(
            zip(split["feature_names"], model["feature_importances"], strict=False)
        ),
        "classification_report": report,
    }


# ---------------------------------------------------------------------------
# Workflow tail: tasks (always re-run, never cached)
# ---------------------------------------------------------------------------


@task(inputs={"eval_result": evaluation})
def deploy_model(eval_result: dict) -> dict:
    """Deploy the evaluated model (asset → task).

    A task that consumes the ``evaluation`` *asset*. It "uploads" the model and
    returns a deployment id. Because it's a task it always re-runs — you never
    want a cached deploy.
    """
    deployment_id = f"iris-{int(eval_result['test_accuracy'] * 10000)}"
    print(
        f"[barca task] deploying model (accuracy={eval_result['test_accuracy']}) -> {deployment_id}"
    )
    return {"deployment_id": deployment_id, "accuracy": eval_result["test_accuracy"]}


@task(inputs={"deploy": deploy_model})
def notify_team(deploy: dict) -> None:
    """Notify the team (task → task = nested task).

    Consumes the ``deploy_model`` *task*. ``barca run notify_team`` scopes the
    run to this whole subtree (assets + deploy + notify).
    """
    print(f"[barca task] deployed {deploy['deployment_id']} — notifying team")
