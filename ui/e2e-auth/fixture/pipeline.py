from barca import asset, task


@asset()
def total():
    return 42


@task()
def say_hello():
    print("hello from authenticated stream")


@task()
def gap_job():
    import time

    time.sleep(3)
    print("finished despite expired replay")
