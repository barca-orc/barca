import time
from pathlib import Path

from barca import asset, task


@asset
def numbers() -> list:
    return [1, 2, 3]


@task(inputs={"nums": numbers})
def say_hello(nums):
    print("hello from e2e", sum(nums))
    # Each call sleeps a different amount so run history has a spread of
    # durations (the node panel's duration histogram needs one).
    counter = Path(__file__).with_name(".calls")
    n = int(counter.read_text()) if counter.exists() else 0
    counter.write_text(str(n + 1))
    time.sleep(0.05 * (n % 6))
