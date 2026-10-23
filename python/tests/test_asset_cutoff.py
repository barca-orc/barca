"""Early cutoff for assets: `@asset(cutoff=True)` (#628).

What reads a cutoff asset folds in the SHA-256 of the bytes it wrote instead of its run hash.
So when it reruns (its code, an upstream or a sensor it reads changed) and writes the same
bytes, what reads it is served from cache: a global step that reruns with unchanged output no
longer reruns every partition key below it. When the bytes change, everything below reruns.

Its consumers are decided in a later phase, after it ran. A dry run or `barca status` cannot
know the bytes of a cutoff asset that would run, so what reads it is `unknown` with reason
`cutoff_pending`.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest

from barca.api import _find_binary

from .test_server_ledger import Server, _free_port, toml

SCRUB = ("BARCA_", "FSSPEC_", "AWS_", "AZURE_", "GOOGLE_", "GCSFS_", "STORAGE_EMULATOR_HOST")

CUSTOMERS = [f"c{i}" for i in range(6)]

PIPELINE = """
import json
import pathlib

from barca import asset, collect, partitions, sensor

HERE = pathlib.Path(__file__).parent
CUSTOMERS = {customers!r}


def _called(what):
    with (HERE / "calls.txt").open("a") as f:
        f.write(what + "\\n")


@sensor()
def feed() -> tuple[bool, float]:
    return True, float((HERE / "rate.txt").read_text())


# A global step every customer reads. It reruns whenever the feed moves; most moves round to
# the same value.
@asset(inputs={{"raw": feed}}{cutoff})
def rates(raw: float) -> dict:
    _called("rates")
    return {{"usd_eur": round(raw, 1)}}


@asset(inputs={{"rates": rates}}, partitions={{"customer": partitions(CUSTOMERS)}})
def billed(customer: str, rates: dict) -> dict:
    _called(f"billed {{customer}}")
    return {{"customer": customer, "eur": 100 * rates["usd_eur"]}}


@asset(inputs={{"b": billed}}, partitions={{"customer": partitions(CUSTOMERS)}})
def invoice(customer: str, b: dict) -> str:
    _called(f"invoice {{customer}}")
    return f"{{customer}}: {{b['eur']:.2f}} EUR"


@asset(inputs={{"all": collect(invoice)}})
def summary(all: dict) -> int:
    _called("summary")
    return len(all)


@sensor()
def regional_feed() -> tuple[bool, dict]:
    return True, json.loads((HERE / "regions.json").read_text())


# Per key: each region's bytes decide its own consumers.
@asset(
    inputs={{"feed": regional_feed}},
    partitions={{"region": partitions(["emea", "apac"])}},
    cutoff=True,
)
def regional(region: str, feed: dict) -> int:
    _called(f"regional {{region}}")
    return feed[region] // 10


@asset(inputs={{"r": regional}}, partitions={{"region": partitions(["emea", "apac"])}})
def regional_report(region: str, r: int) -> str:
    _called(f"regional_report {{region}}")
    return f"{{region}}={{r}}"
"""


def source(cutoff: bool = True) -> str:
    return PIPELINE.format(customers=CUSTOMERS, cutoff=", cutoff=True" if cutoff else "")


def env() -> dict[str, str]:
    out = {k: v for k, v in os.environ.items() if not k.startswith(SCRUB)}
    out["BARCA_POOL_SIZE"] = "2"
    return out


def run(project: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [_find_binary(), *args], cwd=project, env=env(), capture_output=True, text=True
    )


def get(project: Path, target: str, *flags: str) -> dict:
    proc = run(project, "get", target, "pipeline.py", "--json", *flags)
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)


def calls(project: Path) -> list[str]:
    path = project / "calls.txt"
    lines = path.read_text().splitlines() if path.exists() else []
    path.write_text("")
    return sorted(lines)


def keys_run(lines: list[str], node: str) -> int:
    return sum(1 for line in lines if line.split(" ")[0] == node)


def step(result: dict, name: str) -> dict:
    return next(s for s in result["steps"] if s["id"] == f"pipeline.py:{name}")


def set_rate(project: Path, rate: float) -> None:
    (project / "rate.txt").write_text(str(rate))


def set_regions(project: Path, regions: dict) -> None:
    (project / "regions.json").write_text(json.dumps(regions))


def edit_rates(project: Path) -> None:
    """A code change to `rates` that writes the same bytes."""
    path = project / "pipeline.py"
    text = path.read_text()
    old = '    return {"usd_eur": round(raw, 1)}'
    assert old in text
    path.write_text(text.replace(old, '    value = round(raw, 1)\n    return {"usd_eur": value}'))


@pytest.fixture()
def project(tmp_path) -> Path:
    (tmp_path / "pipeline.py").write_text(source())
    set_rate(tmp_path, 0.91)
    set_regions(tmp_path, {"emea": 10, "apac": 20})
    get(tmp_path, "summary")
    get(tmp_path, "regional_report")
    calls(tmp_path)
    return tmp_path


def test_a_global_rerun_with_the_same_bytes_runs_no_partition_key(project):
    # 0.93 rounds to 0.9 as 0.91 did: `rates` reruns (its input moved) and writes the same bytes.
    set_rate(project, 0.93)
    res = get(project, "summary")
    assert calls(project) == ["rates"]
    assert step(res, "rates")["status"] == "ran"
    for name in ("billed", "invoice"):
        assert step(res, name)["status"] == "cached"
        assert step(res, name)["partitions"]["will_run"] == 0
    assert step(res, "summary")["status"] == "cached"
    assert res["steps_executed"] == 2  # the sensor and `rates`

    # New bytes: every key below reruns, and the fan-in.
    set_rate(project, 1.07)
    get(project, "summary")
    lines = calls(project)
    assert keys_run(lines, "billed") == len(CUSTOMERS)
    assert keys_run(lines, "invoice") == len(CUSTOMERS)
    assert "summary" in lines

    # Going back to the earlier bytes serves what was computed from them.
    set_rate(project, 0.92)
    get(project, "summary")
    assert calls(project) == ["rates"]


def test_a_code_edit_with_the_same_bytes_keeps_what_reads_it_cached(project):
    edit_rates(project)
    get(project, "summary")
    assert calls(project) == ["rates"]


def test_a_consumers_own_code_still_counts(project):
    path = project / "pipeline.py"
    path.write_text(path.read_text().replace("EUR\"", "euro\""))
    get(project, "summary")
    lines = calls(project)
    assert keys_run(lines, "invoice") == len(CUSTOMERS)
    assert keys_run(lines, "billed") == 0
    assert "rates" not in lines


def test_dry_run_and_status_say_what_reads_a_cutoff_asset_that_would_run_is_unknown(project):
    edit_rates(project)
    proc = run(project, "get", "summary", "pipeline.py", "--dry-run", "--json")
    assert proc.returncode == 0, proc.stderr
    dry = json.loads(proc.stdout)
    assert step(dry, "rates")["action"] == "run"
    billed = step(dry, "billed")
    assert billed["action"] == "unknown"
    assert billed["reason"] == "cutoff_pending"
    assert "'rates' (cutoff=True)" in billed["detail"]
    assert "runs only if it writes different bytes" in billed["detail"]
    for name in ("invoice", "summary"):
        assert step(dry, name)["action"] == "unknown"
        assert step(dry, name)["reason"] == "cutoff_pending"
        assert "rates" in step(dry, name)["detail"]
    assert dry["summary"]["will_run"] == 2  # the sensor and `rates`
    assert dry["summary"]["unknown"] == 2 * len(CUSTOMERS) + 1

    proc = run(project, "status", "pipeline.py", "--json")
    assert proc.returncode == 0, proc.stderr
    nodes = {n["name"]: n for n in json.loads(proc.stdout)["nodes"]}
    assert nodes["rates"]["cache"]["state"] == "stale"
    assert nodes["billed"]["cache"]["state"] == "unknown"
    assert nodes["billed"]["cache"]["reason"] == "cutoff_pending"
    assert nodes["summary"]["cache"]["reason"] == "cutoff_pending"

    # A dry run executes nothing; the real run then shows the cutoff.
    assert calls(project) == []
    get(project, "summary")
    assert calls(project) == ["rates"]
    # Once it ran, nothing is pending: the dry run is exact again.
    dry = json.loads(run(project, "get", "summary", "pipeline.py", "--dry-run", "--json").stdout)
    assert dry["summary"]["unknown"] == 0
    assert dry["summary"]["will_run"] == 1  # the sensor


def test_a_partitioned_cutoff_asset_cuts_off_per_key(project):
    # emea: 10 -> 12, the same `// 10`; apac: 20 -> 35, new bytes.
    set_regions(project, {"emea": 12, "apac": 35})
    res = get(project, "regional_report")
    assert calls(project) == [
        "regional apac",
        "regional emea",
        "regional_report apac",
    ]
    report = step(res, "regional_report")["partitions"]
    assert report["will_run_keys"] == ["region=apac"]


def test_refresh_of_a_cutoff_asset_stops_the_cascade_at_its_bytes(project):
    res = get(project, "summary", "--refresh", "rates")
    assert step(res, "rates")["reason"] == "refresh"
    assert calls(project) == ["rates"]
    assert step(res, "billed")["status"] == "cached"
    assert "warning" not in step(res, "billed")

    # Named, a consumer still reruns; so does everything under --refresh-all.
    get(project, "summary", "--refresh", "invoice")
    lines = calls(project)
    assert keys_run(lines, "invoice") == len(CUSTOMERS)
    assert keys_run(lines, "billed") == 0
    get(project, "summary", "--refresh-all")
    lines = calls(project)
    assert keys_run(lines, "billed") == len(CUSTOMERS)

    # A refresh that writes new bytes (the feed moved) reruns what reads it.
    set_rate(project, 2.5)
    get(project, "summary", "--refresh", "rates")
    assert keys_run(calls(project), "billed") == len(CUSTOMERS)


def test_turning_cutoff_on_reruns_what_reads_it_once(tmp_path):
    (tmp_path / "pipeline.py").write_text(source(cutoff=False))
    set_rate(tmp_path, 0.91)
    set_regions(tmp_path, {"emea": 10, "apac": 20})
    get(tmp_path, "summary")
    calls(tmp_path)

    # Without cutoff, a rerun of `rates` with the same bytes reruns every key.
    set_rate(tmp_path, 0.93)
    get(tmp_path, "summary")
    assert keys_run(calls(tmp_path), "billed") == len(CUSTOMERS)

    # Turned on: `rates` itself stays cached (cutoff is not part of its run hash), and what reads
    # it reruns once, since its run hash now folds in the bytes.
    (tmp_path / "pipeline.py").write_text(source(cutoff=True))
    get(tmp_path, "summary")
    lines = calls(tmp_path)
    assert "rates" not in lines
    assert keys_run(lines, "billed") == len(CUSTOMERS)

    get(tmp_path, "summary")
    assert calls(tmp_path) == []
    set_rate(tmp_path, 0.94)
    get(tmp_path, "summary")
    assert calls(tmp_path) == ["rates"]


def test_cutoff_must_be_a_boolean_literal(tmp_path):
    (tmp_path / "pipeline.py").write_text(
        "from barca import asset\n\n@asset(cutoff=1)\ndef a() -> int:\n    return 1\n"
    )
    proc = run(tmp_path, "get", "a", "pipeline.py")
    assert proc.returncode == 2, proc.stderr
    assert "cutoff= must be True or False" in proc.stderr


def test_with_a_server_a_cutoff_consumer_is_a_ledger_hit(tmp_path):
    """Run hashes below a cutoff asset fold in its bytes; the ledger answers them as any other."""
    store = tmp_path / "store"
    store.mkdir()
    port = _free_port()
    url = f"http://127.0.0.1:{port}"
    roots = {}
    for name, server_url in (("server", None), ("a", url), ("b", url)):
        root = tmp_path / name
        root.mkdir()
        (root / "pipeline.py").write_text(source())
        (root / "barca.toml").write_text(toml(store, server_url))
        set_rate(root, 0.91)
        set_regions(root, {"emea": 10, "apac": 20})
        roots[name] = root
    server = Server(roots["server"], port)
    try:
        get(roots["a"], "summary")
        assert keys_run(calls(roots["a"]), "billed") == len(CUSTOMERS)

        # B's feed moved but rounds the same: `rates` runs here, every consumer is a hit on
        # the server's ledger, fetched from the store.
        set_rate(roots["b"], 0.93)
        res = get(roots["b"], "summary")
        assert calls(roots["b"]) == ["rates"]
        assert step(res, "billed")["status"] == "cached"
        assert step(res, "summary")["status"] == "cached"

        # A dry run against the ledger: nothing pending once B recorded the new `rates`.
        set_rate(roots["a"], 0.93)
        proc = run(roots["a"], "get", "summary", "pipeline.py", "--dry-run", "--json")
        assert proc.returncode == 0, proc.stderr
        dry = json.loads(proc.stdout)
        assert step(dry, "rates")["action"] == "cached"
        assert step(dry, "billed")["action"] == "cached"
    finally:
        server.stop()


KEYED = """
import json
import pathlib

from barca import asset, partitions_from, sensor

HERE = pathlib.Path(__file__).parent


@sensor()
def feed() -> tuple[bool, str]:
    return True, (HERE / "versions.json").read_text()


# A dict `partitions_from` source that is an asset: with cutoff, each key folds in its own
# value, not the source's run hash, as for a sensor source.
@asset(inputs={"raw": feed}, cutoff=True)
def versions(raw: str) -> dict:
    return json.loads(raw)


@asset(partitions={"customer": partitions_from(versions)})
def per_customer(customer: str, versions: int) -> str:
    with (HERE / "calls.txt").open("a") as f:
        f.write(f"per_customer {customer}\\n")
    return f"{customer}@{versions}"
"""


def test_a_cutoff_dict_source_reruns_only_the_keys_whose_value_changed(tmp_path):
    (tmp_path / "pipeline.py").write_text(KEYED)
    (tmp_path / "versions.json").write_text(json.dumps({"a": 1, "b": 1, "c": 1}))
    get(tmp_path, "per_customer")
    assert keys_run(calls(tmp_path), "per_customer") == 3

    (tmp_path / "versions.json").write_text(json.dumps({"a": 1, "b": 2, "c": 1}))
    get(tmp_path, "per_customer")
    assert calls(tmp_path) == ["per_customer b"]

    # The same values in another order: other bytes, the same value per key.
    (tmp_path / "versions.json").write_text(json.dumps({"c": 1, "b": 2, "a": 1}))
    get(tmp_path, "per_customer")
    assert calls(tmp_path) == []


SNAPSHOT = """
import pathlib

from barca import asset

HERE = pathlib.Path(__file__).parent


def _called(what):
    with (HERE / "calls.txt").open("a") as f:
        f.write(what + "\\n")


# A snapshot of an external system: new bytes for the same code and inputs are expected.
@asset(cutoff=True{deterministic})
def snapshot() -> str:
    _called("snapshot")
    return (HERE / "external.txt").read_text()


@asset(inputs={{"s": snapshot}})
def report(s: str) -> int:
    _called("report")
    return len(s)
"""

NOT_DETERMINISTIC = "produced different bytes for the same code and inputs"


@pytest.mark.parametrize("deterministic", [False, True])
def test_a_nondeterministic_cutoff_asset_reruns_what_reads_it_only_on_new_bytes(
    tmp_path, deterministic
):
    """`deterministic=False` (#634) silences the warning; cutoff (#636) still decides what
    reads the asset by its bytes, so new bytes under the same run hash are never served stale
    and the same bytes keep it cached."""
    (tmp_path / "pipeline.py").write_text(
        SNAPSHOT.format(deterministic="" if deterministic else ", deterministic=False")
    )
    (tmp_path / "external.txt").write_text("one")
    get(tmp_path, "report")
    assert calls(tmp_path) == ["report", "snapshot"]

    # The same run hash, new bytes: what reads it reruns.
    (tmp_path / "external.txt").write_text("three")
    proc = run(tmp_path, "get", "report", "pipeline.py", "--json", "--refresh", "snapshot")
    assert proc.returncode == 0, proc.stderr
    assert calls(tmp_path) == ["report", "snapshot"]
    assert (NOT_DETERMINISTIC in proc.stderr) == deterministic, proc.stderr

    # The same run hash, the same bytes: what reads it stays cached, and no warning.
    proc = run(tmp_path, "get", "report", "pipeline.py", "--json", "--refresh", "snapshot")
    assert proc.returncode == 0, proc.stderr
    assert calls(tmp_path) == ["snapshot"]
    assert NOT_DETERMINISTIC not in proc.stderr
    assert step(json.loads(proc.stdout), "report")["status"] == "cached"
