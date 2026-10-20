"""Column types of the relations the pipeline passes around, for typed empty results."""

ITEM_WEEK = {
    "account_id": "BIGINT",
    "group_id": "BIGINT",
    "item_id": "BIGINT",
    "week": "BIGINT",
    "value": "DOUBLE",
}
GROUP_WEEK = {"account_id": "BIGINT", "group_id": "BIGINT", "week": "BIGINT", "value": "DOUBLE"}
LEDGER = {
    "account_id": "BIGINT",
    "group_id": "BIGINT",
    "week": "BIGINT",
    "units": "DOUBLE",
    "revenue": "DOUBLE",
}
REGISTRY = {
    "account_id": "BIGINT",
    "item_id": "BIGINT",
    "week": "BIGINT",
    "rate": "DOUBLE",
    "kind": "VARCHAR",
}
GROUP_FACTOR = {"group_id": "BIGINT", "factor": "DOUBLE"}
ITEM_WEEK_FACTOR = {"item_id": "BIGINT", "week": "BIGINT", "factor": "DOUBLE"}
GROUP_WEEK_FACTOR = {"group_id": "BIGINT", "week": "BIGINT", "factor": "DOUBLE"}
EXITED = {"account_id": "BIGINT", "week": "BIGINT"}
LONG = {**ITEM_WEEK, "driver": "VARCHAR"}
