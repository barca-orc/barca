---
title: Authentication and Tokens
description: "Require a token on barca serve: turn it on, connect the web UI with a one-use code, create API tokens for laptops and scripts, and revoke them. Login, authentication, access control."
---

`barca serve` has no authentication by default: anyone who can reach its port can read results
and start and cancel runs. It binds `127.0.0.1` unless you pass `--host`. Before you expose it,
either turn on tokens (this page) or keep it on a private network or behind a proxy that
authenticates ([Deploying: Authentication](/deploying/#authentication)).

## Turn tokens on

```bash
barca serve --auth token          # or [server] auth = "token" in barca.toml, or BARCA_SERVER_AUTH=token
```

The flag beats the environment variable, which beats `barca.toml`. With tokens on, every API
request needs a bearer token, health probes and event streams included. Only the web UI's static
files and `GET /auth/status` stay public. Use TLS at a proxy for connections from other
machines.

## Connect the web UI

On the server's host, in the project root:

```bash
barca tokens bootstrap            # prints a one-use code, valid 15 minutes
```

Open the web UI and paste the code on its Connect screen. The connection lasts 12 hours. A new
code replaces any unused one, and running the command again is also how you get back in after
losing every token.

## Create tokens for laptops and scripts

In the web UI, open **API tokens** in the sidebar, name a token (for example `laptop`) and copy
its secret: it is shown once. A token has no expiry unless you set one. A laptop or script sends
it with:

```bash
export BARCA_SERVER_TOKEN=<token>
barca get report --server http://build-box:8274
```

The same page lists tokens and revokes them. Revoking a token closes the event streams it
opened; a change made outside the server, or an expiry, is noticed within a second.

## What a token can do

- **Everything.** Every token grants the server's full capabilities, including creating and
  revoking tokens. There are no accounts or roles. A `--read-only` server still refuses
  pipeline writes but allows token administration.
- **Where they live.** Token state is in `.barca/auth/`, readable only by its owner, shared by
  the project's environments and not part of metadata backups: copy it separately when you
  move a server. If it is unreadable or corrupt, the server refuses every request rather than
  allowing them.
- **Failures.** A missing, invalid, revoked or expired token gets `401` with
  `WWW-Authenticate: Bearer`; a CLI call through `--server` exits 3.

The HTTP routes for scripts (`POST /auth/bootstrap`, `POST /auth/tokens`, `GET` and `DELETE`
on `/auth/tokens`), browser storage and the limits are in
[Server Mode: Opt-in project tokens](/reference/server-mode/#opt-in-project-tokens).
