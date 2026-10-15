"""Protocol v3 compact tables. Only metadata and artifact references cross the wire."""


def encode_table(items, *, hoist=True):
    keys = sorted({key for item in items for key in item})
    header = {}
    if items and hoist:
        header = {key: items[0][key] for key in keys
                  if all(key in item and item[key] == items[0].get(key) for item in items)}
    dims = [key for key in keys if key not in header]
    rows = [[item.get(key) for key in dims] for item in items]
    missing = [[column for column, key in enumerate(dims) if key not in item] for item in items]
    table = {"header": header, "dims": dims, "rows": rows}
    if any(missing):
        table["missing"] = missing
    return table


def decode_table(table):
    if not isinstance(table, dict):
        raise ValueError("table must be an object")
    header, dims, rows = table.get("header"), table.get("dims"), table.get("rows")
    if not isinstance(header, dict) or not isinstance(dims, list) or not isinstance(rows, list):
        raise ValueError("table needs header, dims and rows")
    if any(not isinstance(key, str) for key in dims) or len(set(dims)) != len(dims):
        raise ValueError("table dimensions must be unique strings")
    if set(dims) & header.keys():
        raise ValueError("table dimensions overlap its header")
    missing = table.get("missing", [[] for _ in rows])
    if not isinstance(missing, list) or len(missing) != len(rows):
        raise ValueError("table missing masks must match rows")
    result = []
    for row, absent in zip(rows, missing):
        if not isinstance(row, list) or len(row) != len(dims):
            raise ValueError("table row width must match dimensions")
        if (not isinstance(absent, list) or
            any(type(index) is not int or index < 0 or index >= len(dims) for index in absent) or
            len(set(absent)) != len(absent)):
            raise ValueError("invalid missing columns")
        result.append({**header, **{key: row[index] for index, key in enumerate(dims) if index not in absent}})
    return result


def decode_steps(message):
    steps = decode_table(message["items"])
    if "kwargs" in message:
        kwargs = decode_table(message["kwargs"])
        if len(kwargs) != len(steps):
            raise ValueError("kwargs rows must match steps")
        for step, values in zip(steps, kwargs):
            if values:
                step["direct_kwargs"] = values
    return steps


def encode_results(messages):
    node_ids, statuses, elapsed, details = [], [], [], []
    for message in messages:
        status = message["type"]
        node_ids.append(message["node_id"])
        statuses.append(status)
        if status == "step_completed":
            detail = dict(message["artifact"])
            elapsed.append(detail.pop("elapsed_seconds", None))
            # Optional artifact metadata defaults to absence on Rust's typed peer.
            detail = {key: value for key, value in detail.items() if value is not None and value != []}
            if "json" in detail:
                raise ValueError("artifact data must travel by file reference")
        elif status in ("step_error", "blocked"):
            detail = {key: value for key, value in message.items() if key not in ("type", "node_id", "elapsed")}
            elapsed.append(message.get("elapsed"))
        else:
            raise ValueError("unsupported result status")
        details.append(detail)
    return {"type": "results", "node_ids": node_ids, "statuses": statuses,
            "elapsed_seconds": elapsed, "details": encode_table(details)}


def decode_results(message):
    ids, statuses, elapsed = message.get("node_ids"), message.get("statuses"), message.get("elapsed_seconds")
    details = decode_table(message.get("details"))
    if not all(isinstance(values, list) and len(values) == len(details) for values in (ids, statuses, elapsed)):
        raise ValueError("result arrays must have equal lengths")
    result = []
    for node_id, status, seconds, detail in zip(ids, statuses, elapsed, details):
        if not isinstance(node_id, str) or (seconds is not None and type(seconds) not in (int, float)):
            raise ValueError("invalid result identity or elapsed time")
        if status == "step_completed":
            if "json" in detail:
                raise ValueError("artifact data must travel by file reference")
            if seconds is not None:
                detail["elapsed_seconds"] = seconds
            result.append({"type": status, "node_id": node_id, "artifact": detail})
        elif status in ("step_error", "blocked"):
            item = {"type": status, "node_id": node_id, **detail}
            if status == "step_error":
                item["elapsed"] = seconds
            result.append(item)
        else:
            raise ValueError("unsupported result status")
    return result
