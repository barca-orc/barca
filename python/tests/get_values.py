"""`barca get` prints artifact paths by default (0.25.0). Tests that check a target's value read
it with `-o value`, which puts the value in `final_output` of the JSON result."""


def value_args(args, json: bool = True) -> list:
    """`args` for the barca CLI, with `-o value` (and `--json` unless `json` is false or it is
    there already) added to a `get` that executes and does not choose its output itself."""
    args = list(args)
    if args[:1] != ["get"] or any(a in args for a in ("--dry-run", "-o", "--pretty")):
        return args
    args += ["-o", "value"]
    if json and "--json" not in args:
        args.append("--json")
    return args
