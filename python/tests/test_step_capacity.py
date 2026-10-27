"""`weight=` and the pool's capacity: how many heavy steps run at once (#436).

Each step records when it started and ended; the assertions read the peak number of steps
running at once from those spans, not from wall time.
"""

import json
import os
import re
import subprocess
import textwrap

from barca.api import _find_binary

# Every step appends `name start end` to spans.txt in the project directory.
SPAN = """
import os, time
from pathlib import Path

def span(name, seconds):
    start = time.time()
    time.sleep(seconds)
    with open(Path(__file__).parent / "spans.txt", "a") as f:
        f.write(f"{name} {start} {time.time()}\\n")
"""


def cli(root, *args, code=0, **env):
    proc = subprocess.run(
        [_find_binary(), *args],
        cwd=root,
        env={**os.environ, "BARCA_REMOTE": "off", "BARCA_POOL_SIZE": "4", **env},
        capture_output=True,
        text=True,
        timeout=120,
    )
    assert proc.returncode == code, proc.stderr
    return proc


def write(root, body):
    (root / "p.py").write_text(SPAN + textwrap.dedent(body))
    (root / "spans.txt").unlink(missing_ok=True)


def spans(root):
    out = {}
    for line in (root / "spans.txt").read_text().splitlines():
        name, start, end = line.split()
        out[name] = (float(start), float(end))
    return out


def peak(spans, names=None):
    """The most spans (of `names`, or all) open at one instant."""
    events = []
    for name, (start, end) in spans.items():
        if names is None or name in names:
            events += [(start, 1), (end, -1)]
    # Ends sort before starts at the same instant: touching spans do not overlap.
    events.sort(key=lambda e: (e[0], e[1]))
    running = best = 0
    for _, delta in events:
        running += delta
        best = max(best, running)
    return best


THREE_HEAVY = """
from barca import asset

@asset(weight=2)
def heavy_a() -> int:
    span("heavy_a", 0.6)
    return 1

@asset(weight=2)
def heavy_b() -> int:
    span("heavy_b", 0.6)
    return 2

@asset(weight=2)
def heavy_c() -> int:
    span("heavy_c", 0.6)
    return 3

@asset(inputs={"a": heavy_a, "b": heavy_b, "c": heavy_c})
def total(a: int, b: int, c: int) -> int:
    return a + b + c
"""


def run_hashes(doc):
    return {s["id"]: s.get("run_hash") for s in doc["steps"]}


def test_three_heavy_roots_run_two_at_a_time_with_the_same_outputs_and_cache(tmp_path):
    limited, free = tmp_path / "limited", tmp_path / "free"
    for root in (limited, free):
        root.mkdir()
        write(root, THREE_HEAVY)

    got = json.loads(
        cli(limited, "get", "total", "p.py", "-o", "value", "--json", BARCA_CAPACITY="4").stdout
    )
    assert peak(spans(limited)) == 2
    # The same pipeline with room for all three runs them together.
    unlimited = json.loads(
        cli(free, "get", "total", "p.py", "-o", "value", "--json", BARCA_CAPACITY="100").stdout
    )
    assert peak(spans(free)) == 3

    assert got["final_output"] == unlimited["final_output"] == 6
    assert run_hashes(got) == run_hashes(unlimited)
    assert {s["status"] for s in got["steps"]} == {"ran"}

    # Cached the same way under either capacity, and at another weight.
    again = json.loads(
        cli(limited, "get", "total", "p.py", "-o", "value", "--json", BARCA_CAPACITY="100").stdout
    )
    assert {s["status"] for s in again["steps"]} == {"cached"}
    (limited / "p.py").write_text((limited / "p.py").read_text().replace("weight=2", "weight=3"))
    reweighed = json.loads(
        cli(limited, "get", "total", "p.py", "-o", "value", "--json", BARCA_CAPACITY="4").stdout
    )
    assert {s["status"] for s in reweighed["steps"]} == {"cached"}
    assert run_hashes(reweighed) == run_hashes(got)


def test_plan_dry_run_and_doctor_show_the_capacity_and_the_weights(tmp_path):
    write(tmp_path, THREE_HEAVY)
    (tmp_path / "barca.toml").write_text("[execution]\ncapacity = 4\n")

    plan = json.loads(cli(tmp_path, "plan", "p.py").stdout)
    assert plan["capacity"] == 4
    assert plan["capacity_source"] == "[execution] capacity"
    assert plan["pool_size"] == 4
    assert plan["pool_source"] == "BARCA_POOL_SIZE"
    assert plan["weights"] == {"p.py:heavy_a": 2, "p.py:heavy_b": 2, "p.py:heavy_c": 2}

    plan = json.loads(cli(tmp_path, "plan", "p.py", BARCA_CAPACITY="6").stdout)
    assert (plan["capacity"], plan["capacity_source"]) == (6, "BARCA_CAPACITY")

    dry = json.loads(cli(tmp_path, "get", "total", "p.py", "--dry-run", "--json").stdout)
    assert dry["capacity"] == 4
    weights = {s["id"]: s.get("weight") for s in dry["steps"]}
    assert weights == {
        "p.py:heavy_a": 2,
        "p.py:heavy_b": 2,
        "p.py:heavy_c": 2,
        "p.py:total": None,
    }
    pretty = cli(tmp_path, "get", "total", "p.py", "--dry-run", "--pretty").stdout
    assert "Steps running at once total at most weight 4" in pretty

    doctor = json.loads(cli(tmp_path, "doctor", "p.py", "--json").stdout)
    check = next(c for c in doctor["checks"] if c["name"] == "capacity")
    assert check["data"] == {
        "capacity": 4,
        "pool_size": 4,
        "source": "[execution] capacity",
        "pool_source": "BARCA_POOL_SIZE",
    }


def test_a_bad_capacity_or_weight_is_a_usage_error(tmp_path):
    write(tmp_path, THREE_HEAVY)
    proc = cli(tmp_path, "get", "total", "p.py", code=2, BARCA_CAPACITY="0")
    assert "BARCA_CAPACITY" in proc.stderr
    write(tmp_path, THREE_HEAVY.replace("weight=2", "weight=0", 1))
    proc = cli(tmp_path, "plan", "p.py", code=2)
    assert "weight= must be a whole number of at least 1" in proc.stderr


def test_a_step_heavier_than_the_capacity_runs_alone_with_a_warning(tmp_path):
    write(
        tmp_path,
        """
        from barca import asset

        @asset(weight=5)
        def huge() -> int:
            span("huge", 0.5)
            return 1

        @asset()
        def light_a() -> int:
            span("light_a", 0.5)
            return 2

        @asset()
        def light_b() -> int:
            span("light_b", 0.5)
            return 3

        @asset(inputs={"h": huge, "a": light_a, "b": light_b})
        def total(h: int, a: int, b: int) -> int:
            return h + a + b
        """,
    )
    proc = cli(tmp_path, "get", "total", "p.py", "-o", "value", "--json", BARCA_CAPACITY="2")
    assert json.loads(proc.stdout)["final_output"] == 6
    assert (
        "[barca] warning: 'huge' has weight=5, more than the capacity 2 (BARCA_CAPACITY); "
        "it runs alone" in proc.stderr
    )
    got = spans(tmp_path)
    start, end = got["huge"]
    for other in ("light_a", "light_b"):
        o_start, o_end = got[other]
        assert o_end <= start or o_start >= end, f"{other} overlapped huge: {got}"
    # The two light ones still ran together.
    assert peak(got, {"light_a", "light_b"}) == 2
    plan = cli(tmp_path, "plan", "p.py", BARCA_CAPACITY="2")
    assert "more than the capacity 2" in plan.stderr


def chain(prefix, length, seconds):
    """`length` light tasks, each reading the one before."""
    out = []
    for i in range(1, length + 1):
        inputs = f'inputs={{"x": {prefix}{i - 1}}}' if i > 1 else ""
        arg = "x: int" if i > 1 else ""
        out.append(
            f"@task({inputs})\n"
            f"def {prefix}{i}({arg}) -> int:\n"
            f'    span("{prefix}{i}", {seconds})\n'
            f"    return {i}\n"
        )
    return "\n".join(out)


def test_a_heavy_step_is_not_overtaken_by_light_steps_ready_after_it(tmp_path):
    """Two chains of light steps keep the capacity busy. Were light steps let past a heavy one
    waiting for room, it would wait for both chains to end. The pool (4) is larger than the
    capacity (2), so each chain is a stream of its own and only the capacity holds steps back;
    `x1 -> heavy` is a third stream, so `heavy` becomes ready in the middle of the phase."""
    write(
        tmp_path,
        "from barca import task\n\n"
        + chain("a", 6, 0.2)
        + "\n"
        + chain("b", 6, 0.35)
        + "\n"
        + chain("x", 1, 0.3)
        + """
@task(inputs={"x": x1}, weight=2)
def heavy(x: int) -> int:
    span("heavy", 0.2)
    return x
""",
    )
    plan = json.loads(cli(tmp_path, "plan", "p.py").stdout)
    assert len(plan["phases"]) == 1, plan
    cli(tmp_path, "run", "a6,b6,heavy", "p.py", BARCA_CAPACITY="2")
    got = spans(tmp_path)
    assert len(got) == 14
    heavy_start, heavy_end = got["heavy"]
    heavy_ready = got["x1"][1]
    # Each light step becomes ready when the one before it in its chain ends.
    for prefix in "ab":
        for i in range(2, 7):
            ready = got[f"{prefix}{i - 1}"][1]
            start = got[f"{prefix}{i}"][0]
            if ready > heavy_ready + 0.05:
                assert start >= heavy_start, f"{prefix}{i} overtook heavy: {got}"
    # Not vacuous: it started while both chains still had steps to run.
    assert heavy_start < min(got["a6"][0], got["b6"][0]), got
    for name, (start, end) in got.items():
        if name != "heavy":
            assert end <= heavy_start or start >= heavy_end, f"{name} overlapped heavy: {got}"
    assert peak(got) <= 2


def test_partition_keys_and_parallel_branches_take_their_nodes_weight(tmp_path):
    write(
        tmp_path,
        """
        from functools import partial
        from barca import asset, task, parallel, partitions

        @asset(partitions={"k": partitions(["a", "b", "c", "d"])}, weight=2)
        def per_key(k: str) -> str:
            span(f"key_{k}", 0.5)
            return k

        @task()
        def branch(i: int) -> int:
            span(f"branch_{i}", 0.5)
            return i

        @task(weight=2)
        def fan_out() -> list:
            return parallel(*[partial(branch, i) for i in range(4)])
        """,
    )
    cli(tmp_path, "get", "per_key", "p.py", BARCA_CAPACITY="4")
    got = spans(tmp_path)
    assert set(got) == {"key_a", "key_b", "key_c", "key_d"}
    assert peak(got) == 2

    (tmp_path / "spans.txt").unlink()
    out = json.loads(cli(tmp_path, "run", "fan_out", "p.py", "--json", BARCA_CAPACITY="4").stdout)
    assert out["status"] == "success"
    got = spans(tmp_path)
    assert set(got) == {f"branch_{i}" for i in range(4)}
    assert peak(got) == 2


def test_a_heavy_step_that_raises_keeps_its_worker_and_frees_its_capacity(tmp_path):
    """#436 meets #523: a failed heavy key gives back its weight while its worker stays."""
    write(
        tmp_path,
        """
        from barca import asset, partitions

        KEYS = [f"k{i}" for i in range(8)]

        @asset(weight=2, partitions={"key": partitions(KEYS)})
        def heavy(key: str) -> str:
            span(key, 0.3)
            if int(key[1:]) % 2:
                raise ValueError(f"{key} fails on purpose")
            return key
        """,
    )
    proc = cli(
        tmp_path,
        "get",
        "heavy",
        "p.py",
        "--json",
        code=1,
        BARCA_CAPACITY="4",
        BARCA_TRACE_TIMING="1",
    )
    found = re.findall(
        r"workers started (\d+), failed steps that kept their worker (\d+), steps dispatched (\d+)",
        proc.stderr,
    )
    assert found, proc.stderr
    started, kept, dispatched = (int(n) for n in found[-1])
    # Every key ran, two at a time: a failed key's weight was given back, or the run would stop.
    assert len(spans(tmp_path)) == 8
    assert peak(spans(tmp_path)) == 2
    # The four failures kept their workers, and nothing was sent twice.
    assert kept == 4, proc.stderr
    assert started <= 2, proc.stderr
    assert dispatched == 8, proc.stderr
    assert sorted(json.loads(proc.stdout)["final_output"]) == [
        "key=k0",
        "key=k2",
        "key=k4",
        "key=k6",
    ]
