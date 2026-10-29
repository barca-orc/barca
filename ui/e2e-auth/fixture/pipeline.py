from barca import asset, task


@asset()
def total():
    return 42


@task()
def say_hello():
    print("hello from authenticated stream")
