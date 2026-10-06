---
title: Agent Skill
description: SKILL.md, the short guide an AI agent loads once before driving barca.
---

Barca ships a short [Agent Skills](https://agentskills.io/) file for AI coding agents:
[`SKILL.md`](https://github.com/barca-orc/barca/blob/main/SKILL.md) at the repository root. It
is about 1500 tokens and covers what an agent needs before its first command:

- when to use barca, and the loop: `barca list` / `barca status` to discover, `barca get` (assets)
  and `barca run` (tasks) to execute, `--dry-run` to preview;
- no file arguments needed: barca reads the whole project (`barca docs discovery`);
- the argument order (target, then files) and comma-separated targets and `--refresh`, which
  cascades downstream unless `--no-cascade`;
- the output contract: the JSON result on stdout (its last line), the error envelope as the last
  line of stderr, and exit codes 0 / 1 step failed / 2 usage / 3 infra / 130 cancelled;
- `--agent`, `--fields`, and `set -o pipefail` before piping into `jq`;
- guardrails: never edit `.barca/`, never import the user's modules to inspect outputs (use
  `barca status`), and one spelling per task.

The same text is compiled into the binary, so it works offline and matches the installed version:

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
