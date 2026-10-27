"""Dagster: resilience / anti-pileup workload (script mode = sequential)."""

import json
import os
import time
from pathlib import Path
from dagster import asset, materialize, RetryPolicy

_STATE = Path(os.environ.get("BENCH_STATE", "/tmp/bench_pileup_state"))
_STATE.mkdir(parents=True, exist_ok=True)


def _poison_attempt():
    c = _STATE / "poison"
    n = int(c.read_text()) if c.exists() else 0
    c.write_text(str(n + 1))
    return n


def _work():
    time.sleep(50 / 1000.0)


@asset(name="h0")
def h0():
    _work()
    return 0


@asset(name="t0")
def t0(h0):
    _work()
    return h0


@asset(name="h1")
def h1():
    _work()
    return 1


@asset(name="t1")
def t1(h1):
    _work()
    return h1


@asset(name="h2")
def h2():
    _work()
    return 2


@asset(name="t2")
def t2(h2):
    _work()
    return h2


@asset(name="h3")
def h3():
    _work()
    return 3


@asset(name="t3")
def t3(h3):
    _work()
    return h3


@asset(name="h4")
def h4():
    _work()
    return 4


@asset(name="t4")
def t4(h4):
    _work()
    return h4


@asset(name="h5")
def h5():
    _work()
    return 5


@asset(name="t5")
def t5(h5):
    _work()
    return h5


@asset(name="h6")
def h6():
    _work()
    return 6


@asset(name="t6")
def t6(h6):
    _work()
    return h6


@asset(name="h7")
def h7():
    _work()
    return 7


@asset(name="t7")
def t7(h7):
    _work()
    return h7


@asset(retry_policy=RetryPolicy(max_retries=2, delay=0.5))
def poison_head():
    n = _poison_attempt()
    if n < 2:
        raise RuntimeError(f"poison transient {n}")
    return n


@asset
def poison_tail(poison_head):
    _work()
    return True


ALL_ASSETS = [
    h0,
    h1,
    h2,
    h3,
    h4,
    h5,
    h6,
    h7,
    t0,
    t1,
    t2,
    t3,
    t4,
    t5,
    t6,
    t7,
    poison_head,
    poison_tail,
]

if __name__ == "__main__":
    t0 = time.perf_counter()
    result = materialize(ALL_ASSETS)
    elapsed = time.perf_counter() - t0
    print(
        json.dumps(
            {"elapsed_seconds": round(elapsed, 6), "steps_executed": 18, "success": result.success},
            indent=2,
        )
    )
