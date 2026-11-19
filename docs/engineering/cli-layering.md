# The CLI's layers

How `crates/barca-cli` is laid out (#1056, items E2 and E3), and the rule a test holds it to.
Non-normative: the CLI's surface is specified by `barca docs contract` and its snapshots, and
none of this changes it.

## One request, one dispatch

The CLI is one host of barca's commands and `barca serve` is another. A command line becomes
a request once, and one place decides where the request runs:

```text
argv ──clap──> Cli ──dispatch::place──> Target ──Request::of(cli, side)──> Request ──> backend
                         │                 │                                              │
                     route::ROUTES     Here | Server                    commands::perform (this process)
                                                                         client::perform   (a server)
```

- **`route::ROUTES`** is the table of where each command can run (below). The commands that
  take `--server` and `--local`, the usage error for those flags on any other command, whether
  the project's socket is looked for, what a configured server is used for and the usage error
  of a command that needs a server are all read from it. `route_tests.rs` holds it to clap (a
  command has `--server` exactly when its row says so) and to `barca docs server-mode`.
- **`dispatch::place`** resolves the target in the order a user sees on stderr: `--server`,
  then the project root, then this project's own `barca serve` by its socket
  (`.barca/serve.sock`; one or two `stat` calls when there is none, none at all for a command
  whose row never goes there, for `--local`, and for a command a step starts with
  `BARCA_WORKER` set), then a configured server (`BARCA_SERVER`, `[server] url`), then this
  machine.
- **`Request::of`** is the one place arguments are checked. Every usage error a command can
  end with before it does anything comes from it. It takes the side the request is going to,
  because one check differs: on this machine the files are `.py` files and `get`, `run` and
  `status` need one; a server takes a directory or none (it has its deployed files). `get -o
  url` and `history --reconcile` are refused for a server. The checks run in a fixed order, so
  a command line with two mistakes reports the same one wherever it runs.
- **The backends** each have one `perform(request, ...)`: `commands` calls `barca_core`,
  `client` calls the server's routes. `doctor` is the one command both take part in (this
  machine's checks, then the server's), so `dispatch` composes it.

The target is resolved before the request is built, not after. A configured server is named on
stderr (`[barca] server <url> (<source>)`) before a usage error of the command it would have
answered, and the order of those two lines is part of what the command prints.

## Where each command can run

Printed from the table by `cargo test -p barca route::tests::print_the_table -- --nocapture`;
the test fails when this copy is out of date.

| Command | `--server` | This project's `barca serve` | A configured server | This machine |
|---|---|---|---|---|
| `get` | runs there | runs there | the run's ledger | runs here |
| `run` | runs there | runs there | the run's ledger | runs here |
| `status` | runs there | runs there | answers | runs here |
| `list` | runs there | not used | not used | runs here |
| `history` | runs there | runs there | answers | runs here |
| `stats` | runs there | runs there | answers | runs here |
| `logs` | runs there | runs there | answers | runs here |
| `sql` | views from it, query here | views from it, query here | views from it, query here | runs here |
| `queue` | runs there | runs there | answers | usage error (needs a server) |
| `pause` | runs there | runs there | answers | usage error (needs a server) |
| `resume` | runs there | runs there | answers | usage error (needs a server) |
| `cancel` | runs there | runs there | answers | usage error (needs a server) |
| `pin` | runs there | runs there | answers | runs here |
| `unpin` | runs there | runs there | answers | runs here |
| `gc` | runs there | runs there | not used | runs here |
| `doctor` | checked too | not used | checked too | runs here |
| `history --reconcile` | usage error (records a run here) | not used | not used | runs here |
| `plan` | usage error | not used | not used | runs here |
| `state` | usage error | not used | not used | runs here |
| `serve` | usage error | not used | not used | runs here |
| `tokens` | usage error | not used | not used | runs here |
| `docs` | usage error | not used | not used | runs here |
| `version` | usage error | not used | not used | runs here |

`history --reconcile` has a row of its own because it is the one flag that changes where a
command can run. `sql` with any server asks the server which result each view reads and runs
the query here; a query over barca's own tables alone is sent to the server.

## The module order

`crates/barca-cli/src/layering_tests.rs` reads every `crate::<module>` path in non-test code and
fails when a module names one that is not in a row above its own. A row never uses itself or a
row below it, so the modules cannot form a cycle. There is no allowlist.

| Row | Modules | What they are |
|---|---|---|
| 0 | `bounded`, `docs`, `error` | `--limit` and `--fields` vocabulary, the manual, the error envelope |
| 1 | `output`, `runtime` | the output rule (JSON or text), the process's one lazy Tokio runtime |
| 2 | `args` | the clap schema and the help text |
| 3 | `input` | checks of single arguments, the project root, projections of the parsed command |
| 4 | `route` | where each command can run: the table |
| 5 | `request` | a command as a request, checked once (`Request::of`) |
| 6 | `print` | what a command prints, wherever it ran |
| 7 | `client`, `commands` | the two backends: a server, this machine |
| 8 | `dispatch` | the one place that picks between them |
| 9 | `main` | parse, the commands that need nothing (`version`, `docs`), exit |

`contract` and `snapshot` are compiled for tests only and may read anything.

What the order holds in place:

- `client` and `commands` share a row, so neither can import the other. What both need is
  above them: the request types in `request`, the printers in `print`. What needs both is
  below them, in `dispatch`.
- `error` is a leaf. It used to build the clap command to suggest a mistyped flag
  (`error -> args`) and to read the output rule for errors raised before the arguments parse
  (`error -> output`), while `output` ended the process through `error` and `args` embedded
  `output`'s flags: a cycle of three. The clap command is now passed in (`from_clap(e, argv,
  root)`) and the pre-parse output rule lives in `output`.

Before this change the crate had two cycles: `args -> output -> error -> args`, and
`client <-> commands` (`commands/doctor.rs` called the client; the client used the local
commands' printers and request types).

## Where the printers are

`print` holds the printers the CLI keeps: `emit` (writes what `barca_core::report` rendered),
`print_groups` (`list --groups`), `print_gc`, the four `pin` printers and the `logs` line
printers with `-f`'s closing line and exit code. They stay in the CLI, not beside the
renderers in `barca_core::report`, for three reasons. A server prints none of them: it answers
with the typed value (`GcResult`, `Versions`, `Pinned`, the groups, the log entries) as JSON,
and the `--json` half of each printer is one `serde_json` call on that value. They write
straight to stdout and stderr, where `report`'s renderers return chunks, so moving them means
rewriting them. And whether text rendering belongs in the CLI at all is an open decision of
the plan (C5).

## What is left

- `client/inspect.rs` still adapts some answers to core shapes before rendering: `/list` nodes
  into `AssetSummary`, the run detail's steps and page, the files filter with the summary
  recomputed, the table of `sql`. Each wants a typed view the server and the CLI share
  (`barca_core::served`), which is the views track's, not a move within this crate.
- `SERVER_QUERY_ROWS` in `client/inspect.rs` repeats the server's row limit for
  `POST /sql/query`.
- `-o url` signing (`commands/get.rs`) and the local `logs -f` tail loop (`commands/logs.rs`)
  are command logic a server route would also need. No route needs them yet, so they stay.
- `gc --max-size` is parsed by each backend where it uses it, not in `Request::of`: the local
  command parses it after the request is built either way, and the client checks it before it
  asks the server.
- A command a configured server answers (`status`, `stats`, `pin`, `unpin` without `--server`)
  sends the files discovery found on this machine, where this project's own server and
  `--server` send none unless they were typed. Kept as it was.
