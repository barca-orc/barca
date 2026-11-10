---
title: "barca docs"
description: "The built-in manual, for the installed version."
---

The manual, compiled into the binary, so it works offline and describes the installed version.
Every command's `--help` ends with examples.

## Usage

```txt wrap
barca docs [topic] [--all] [--json]
```

```bash
barca docs                    # topic index with one-line summaries
barca docs types              # one topic as markdown (output formats, annotations, duckdb)
barca docs examples/duckdb    # a runnable example pipeline
barca docs skill              # the agent skill (SKILL.md), with its frontmatter
barca docs --all              # every topic in one stream
barca docs --json             # topic index as JSON; add a topic for its full text
barca docs --fields name      # JSON index with only topic names (name, summary, content)
barca docs cache --pretty     # markdown, the default even when piped
```

[Agent Skill](/reference/agent-skill/) is the short [agent skill](/reference/agent-skill/) an AI agent loads once.
[CLI Contract](/reference/cli-contract/) is the [CLI contract](/reference/cli-contract/). [Scripts and AI Agents](/reference/agents/)
describes the output contract for scripts and AI agents.

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/exit-codes/)
