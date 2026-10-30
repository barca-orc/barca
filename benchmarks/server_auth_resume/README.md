# Authentication and event replay cost

Run inside an owned worktree, using its fast-profile native binary:

```bash
.venv/bin/python benchmarks/server_auth_resume/bench.py \
  --binary .venv/bin/barca --workdir target-local/auth-resume-cost --samples 200 --tokens 1000
cargo test --profile fast -p barca-server replay_retention_and_reconnect_cost_probe \
  -- --ignored --nocapture
```

The HTTP work directory must not exist. Each mode uses a separate project and real server.
The script measures `/health` and cursor reconnect response headers over new loopback TCP
connections, with auth off and then token mode. It cancels its task and stops its servers in
`finally`. Secrets remain in memory and are omitted from results. Token-file bytes and mtime
must remain unchanged during measured reads; code uses one locked bounded state file, with
no directory walk, metadata hash or last-used write. The full-list probe uses the last of
1,000 ordinary tokens plus the reserved bootstrap connection.

The explicitly ignored Rust probe emits 1,024 events carrying 256 KiB each across eight
attempts. It checks per-run/global retention limits, measures 1,000 cursor attachments through
the first shared frame, checks that reconnects do not increase retained bytes, and verifies
that dropping all channels releases the hub's payload and index. RSS is the entire process
on Linux, not a precise allocator accounting of replay overhead. Serialized-byte limits do
not include HTTP buffers, live progress or worker memory.

## 2026-10-29 measurements

These are single warm fast-profile local runs, not release-wheel, TLS, cloud-proxy or
cold-filesystem measurements. Off/token runs are sequential rather than randomized; small
differences in the one-token run are noise and do not establish negative auth overhead.
Raw JSON records the native binary fingerprint, platform, samples and limits.

| Probe | Median | p95 |
|---|---:|---:|
| Health, auth off (full-list comparison) | 193 µs | 273 µs |
| Health, 1,000 ordinary tokens | 1,201 µs | 1,865 µs |
| Reconnect headers, auth off (full-list comparison) | 115 µs | 282 µs |
| Reconnect headers, 1,000 ordinary tokens | 1,201 µs | 1,467 µs |
| Health, one reserved connection | 150 µs | 277 µs |
| Reconnect headers, one reserved connection | 170 µs | 299 µs |
| In-process retained cursor attachment/first frame | 170 ns | 180 ns |

The large-event probe emitted 256 MiB of log payload in 84 ms. Retention plateaued at
66,870,435 serialized bytes (255 events), below 64 MiB; process RSS was 81,084 KiB. These
numbers establish this fixture's bounded retention and reconnect cost, not a universal
memory or latency guarantee. The ignored probe makes no timing assertion in ordinary CI.

Raw results: [one connection](2026-10-29-one-token-fast-local.json),
[full token list](2026-10-29-full-list-fast-local.json),
[retention](2026-10-29-retention-fast-local.json).
