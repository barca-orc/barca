---
title: Agent Skill
description: SKILL.md, the short guide an AI coding agent loads once before it runs barca commands.
---

Barca ships a short [Agent Skills](https://agentskills.io/) file for AI coding agents:
[`SKILL.md`](https://github.com/barca-orc/barca/blob/main/SKILL.md) at the repository root. In
the repository it is a short guide and covers what an agent needs before its first command:

- when to use barca, and the loop: `barca list` and `barca status` to see what exists and what is
  cached, `--dry-run` to preview, `barca get` (assets) and `barca run` (tasks) to execute, and
  `barca sql` to look at a cached result without running anything;
- no file arguments needed: barca reads the whole project (`barca docs discovery`);
- the argument order (target, then files) and comma-separated targets and `--refresh`, which
  cascades downstream unless `--no-cascade`;
- the output contract: the JSON result on stdout (its last line), the error envelope as the last
  line of stderr, and exit codes 0 / 1 step failed / 2 usage / 3 infra / 130 cancelled;
- `--agent`, `--fields`, and `set -o pipefail` before piping into `jq`;
- guardrails: never edit or delete files under `.barca/`, never import the user's modules to
  inspect outputs (use `barca status` or `barca sql`), force a recompute with `--refresh` and not
  by deleting files, and one spelling per task.

The same text is compiled into the binary, so it works offline and describes the installed
version (`barca docs skill` in 0.18.0 prints exactly the repository's `SKILL.md` at that tag):

```bash
barca docs skill
```

To install it as a skill, save that output where your agent loads skills, for example for Claude
Code in a project:

```bash
mkdir -p .claude/skills/barca
barca docs skill > .claude/skills/barca/SKILL.md
```

The longer reference, with the full error envelope, several-target output and bounded output, is
`barca docs agents` (see also the [CLI reference](/reference/cli/)). Every command, flag and JSON
schema, marked stable or experimental, is in the [CLI contract](/reference/cli-contract/).

## What the skill does not cover

The skill is short on purpose. Three things an agent working with data should also know, from
`barca docs sql`, `barca docs partitions` and `barca docs cache`:

- `barca sql` returns at most 100 rows unless you pass `--limit N` or `--all`, and its JSON is
  one indented document (`columns`, `rows`, `total`, `truncated`), not one line. Only parquet and
  json results are views; a pickled result exits 2. Install SQL support with `uv add 'barca[sql]'`.
- The skill does not mention `--env`. Results cached with `--env dev` are visible only to
  commands that pass `--env dev` (or run with `BARCA_ENV=dev`); without it `barca status` reports
  `never_run` and `barca sql` says the node has no result yet.
- `barca sql` runs the statement you give it in DuckDB. A statement that writes files, such as
  `COPY ... TO 'file.csv'`, writes them, relative to the project root. Use `select`.

Inspection and previews can synchronize optimistic shared history. Evaluated partition
expressions can import a pipeline while resolving keys; SQL `COPY ... TO` can explicitly
write files. The skill distinguishes these effects from executing pipeline steps.
