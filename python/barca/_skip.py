"""The explicit no-work outcome of a task invocation."""


class Skip(Exception):
    """Raise from a task to skip this invocation with a recorded reason.

    A skip writes no result and does not consume retries or change the schedule's
    failure streak. Assets, sensors, imports and parallel branches cannot skip.
    """

    def __init__(self, reason: str) -> None:
        if not isinstance(reason, str):
            raise TypeError("Skip reason must be a string")
        self.reason = reason
        super().__init__(reason)
