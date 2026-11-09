---
title: "barca tokens"
description: "Print a one-use code to connect the web UI or a laptop to a server that runs with --auth token (experimental)."
---

Run on the server's host, in the project root, when the server requires tokens
(`barca serve --auth token`). It prints a one-use bootstrap code, valid 15 minutes, that the web
UI's Connect screen accepts; from there you create named API tokens. A new code replaces any
unused one. The code goes to stdout and nothing else is printed there.

```
$ barca tokens bootstrap
<one-use code>
```

## Usage

```txt wrap
barca tokens bootstrap
```

The steps from a code to a token a laptop or script uses are in
[Authentication and Tokens](/auth/). The routes, storage and
limits are in [Server Mode: Opt-in project tokens](/reference/server-mode/#opt-in-project-tokens).

---

[All commands](/reference/cli/) · [Output format](/reference/cli/#output-format) · [Errors and exit codes](/reference/cli/#errors-and-exit-codes)
