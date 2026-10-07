---
title: Philosophy
description: The design goals behind barca, the reason for each, and what each one costs.
---

Barca runs Python functions as a dependency graph and caches their results. It suits data
pipelines that run on one machine and want dependency ordering, caching and parallel execution
without operating a scheduler service. It is embedded in the way DuckDB or SQLite are: a
binary and a directory inside your project, not a service you deploy.

## The pipeline is ordinary Python

The decorators (`@asset`, `@sensor`, `@task`) return the function unchanged, so a pipeline
file can be imported, called and tested without barca.

Cost: barca knows only what the decorators declare. A dependency that is not in `inputs=` is
not in the graph.

## Planning reads source and does not import it

The binary parses your files with ruff's Python parser, so planning has no import side effects
and does not wait for your packages to load. (The exception: a non-literal
`partitions(<expression>)` is evaluated by Python at plan time.)

Cost: decorators, `inputs=` and `freshness=` must be written literally. `inputs` built in a
loop is not seen. Some code changes are not seen by the cache either, a star import for
example; `barca docs cache` lists them.

## No server and no configuration by default

`barca get` plans, runs and exits. State is the `.barca/` directory. `barca.toml` is optional.

Cost: nothing happens when no command is running. Cron schedules need `barca serve`
([Scheduling](/scheduling/)), which has no authentication of its own
([Deploying](/deploying/)).

## One install

`pip install barca` installs the binary, the decorators and the worker. The worker needs only
the standard library; parquet and remote storage are extras.

Cost: a wheel must exist for your platform. 0.18.0 has wheels for macOS on Apple Silicon and
x86-64 Linux with glibc, and requires Python 3.12 or later.

## Rust plans, Python executes

Parsing, hashing, cache lookup and the metadata database are in the Rust binary, so a command
with nothing to run returns without starting Python. Your functions run in ordinary Python
processes from your environment. Measured run times are on the
[framework comparison](/comparisons/framework-comparison/) page.

Cost: two languages, and a protocol between them ([Architecture](/architecture/)).

## A step is a function of its declared inputs

A result is cached by run hash: a hash of the function's code, the helper code it reaches and
its inputs. If none of those changed, the function does not run. Every result is written to a
file, and the next step reads that file.

Cost: an asset that reads a file or a bucket in its own body is computed once and then served
from cache; outside data has to come in through a `@sensor`. Every step boundary pays for
serialisation.

## What barca does not do

- Distribute a run across machines. A remote store shares results between machines only.
- General workflows: approval gates, or workflows that wait on external events.
- Act on `Always` or `Manual` freshness. Only `Schedule` has an effect today
  ([Core constraints](/core-constraints/#freshness-declarations)).
