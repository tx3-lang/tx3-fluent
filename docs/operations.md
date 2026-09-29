# Operating Tx3 Fluent

This guide is for whoever runs `fluent serve --http` as a service: what the
server measures, how to set `[limits]` from those measurements, and what the
logs contain.

## Limits

Transaction tools (every tool except `fluent_get_skill` and
`fluent_inspect_address`) run under three limits, in this order:

1. **Global cutoff** — `limits.global_cutoff_secs` (default 45). The whole
   call, including waiting for a slot and the quota check, must finish in
   time, or it fails as `resolver_timeout` with `details.timeout_secs` set to
   the cutoff. The engine separately bounds each resolver call by
   `limits.resolver_timeout_secs` (default 30), which must not exceed the
   cutoff.
2. **Daily quota** — `limits.per_user_daily_quota` (default 200). With a
   `[store]`, each principal (the OIDC `sub`, or `token` in `token` mode) may
   make that many calls per UTC day. The next call fails as
   `quota_exhausted` with `details.limit` and `details.resets_at`, the next
   UTC midnight in RFC 3339; it is not counted and never reaches the
   resolver. Counts are kept in the store's `quota_usage` table, so they
   survive restarts and hold across processes sharing the file. Without a
   `[store]`, or over stdio, calls are not metered.
3. **Concurrency gate** — `limits.max_concurrent_resolutions` (default 8).
   At most that many preparations run at once across the server. A call
   waits up to 5 seconds for a slot, then fails as `resolver_unavailable`
   with `details.reason = "server_busy"`. Clients may retry it.

A call counts against the quota once it passes the scope check (a tool of a
registration the user has not selected fails first, as
`registration_unavailable`, and is not counted). It counts whatever happens
next, including when the gate turns it away as busy; only the rejected call
itself is not counted.

## Metrics

`GET /metrics` serves the Prometheus text format. It answers a direct
loopback caller without credentials. Any other caller must send
`Authorization: Bearer <token>` with the token in `server.metrics_token_env`,
or, in `token` mode, the API token. A request carrying `Forwarded` or
`X-Forwarded-For` came through a proxy and always needs a token, so a reverse
proxy on the same host does not publish the metrics. On Kubernetes, set
`metrics_token_env` and give Prometheus the token as a bearer credential.

| Series | Kind | Labels | Meaning |
| --- | --- | --- | --- |
| `fluent_prepare_total` | counter | `registration`, `tx`, `outcome` | Transaction tool calls. `outcome` is `ok` or the error code, such as `invalid_arguments`, `resolver_timeout`, `resolver_unavailable` or `quota_exhausted`. |
| `fluent_prepare_duration_seconds` | histogram | `registration`, `tx`, `outcome` | Duration of those calls, limits included. Buckets from 50 ms to 60 s. |
| `fluent_quota_rejections_total` | counter | | Calls refused as `quota_exhausted`. |
| `fluent_inflight_resolutions` | gauge | | Preparations holding a slot right now. |
| `fluent_sessions_active` | gauge | | Initialized MCP sessions. Sessions live in memory and end on restart. |
| `process_cpu_seconds_total`, `process_resident_memory_bytes`, `process_virtual_memory_bytes`, `process_threads`, `process_open_fds`, … | | | The server process, measured on each scrape. |

Labels hold catalog identifiers and error codes only, so the number of series
grows with the catalog, not with traffic. No series carries a user, an
address or an argument.

Useful queries:

```promql
# Success rate over five minutes.
sum(rate(fluent_prepare_total{outcome="ok"}[5m])) / sum(rate(fluent_prepare_total[5m]))

# p95 latency of successful calls, per registration.
histogram_quantile(0.95,
  sum by (registration, le) (rate(fluent_prepare_duration_seconds_bucket{outcome="ok"}[5m])))

# Calls turned away as busy, and cut off.
sum(rate(fluent_prepare_total{outcome="resolver_unavailable"}[5m]))
sum(rate(fluent_prepare_total{outcome="resolver_timeout"}[5m]))

# Quota rejections per hour.
increase(fluent_quota_rejections_total[1h])
```

`resolver_unavailable` covers both a busy server and an unreachable resolver.
The logs tell them apart: a busy call logs `no resolution slot freed up;
server busy`, a resolver failure logs `resolver unavailable` with the
network.

## Setting limits from measurements

Start from the defaults, then adjust from a week of metrics:

- **`max_concurrent_resolutions`** — Watch `fluent_inflight_resolutions` and
  the rate of busy rejections. If the gauge sits at the limit and busy
  rejections appear while CPU and memory have headroom and the resolver's
  latency stays flat, raise the limit. If resolver latency (the
  `fluent_prepare_duration_seconds` p95 of `ok` calls) climbs as the gauge
  rises, the resolver is the bottleneck: keep or lower the limit instead. A
  useful estimate is peak calls per second × p95 duration in seconds, plus
  headroom.
- **`resolver_timeout_secs`** — Set it above the p99 duration of `ok` calls
  with margin. If `resolver_timeout` outcomes appear while the p99 is well
  below the timeout, the resolver is failing intermittently rather than
  being slow.
- **`global_cutoff_secs`** — Keep it a few seconds above
  `resolver_timeout_secs` plus the 5-second admission wait, so a call that
  waited for a slot can still finish. MCP clients time tool calls out on
  their own; keep the cutoff below the shortest client timeout you support.
- **`per_user_daily_quota`** — Compare `fluent_quota_rejections_total` with
  the number of active users. Rejections from a handful of principals point
  at automation or abuse; revoke with `fluent admin revoke` rather than
  lowering the quota for everyone. Widespread rejections mean the quota is
  too low for normal use. Each call costs one resolver request, so the quota
  times the number of users bounds the daily resolver load.

The limits are read at startup; change them in the file or with
`FLUENT_LIMITS__*` overrides and restart.

### Load runs

[`scripts/load_run.py`](../scripts/load_run.py) (Python standard library only)
serves a scripted resolver and drives the server with concurrent clients:

```sh
scripts/load_run.py stub --port 9999 --delay 0.2 &
FLUENT_API_TOKEN=load-token fluent serve --http --config load.toml &   # trp_url = "http://127.0.0.1:9999"
scripts/load_run.py drive --url http://127.0.0.1:8080 --token load-token --clients 50 --calls 10
```

`drive` opens one session per client, has all of them call the transfer tool
at once, and prints the latency histogram, the outcomes and the server's
`fluent_*` series. Each client needs its own session and request ids:
concurrent requests that share a JSON-RPC id in one session are not answered
reliably, so a tool like `oha` or `hey` replaying one request is not a valid
load test. MCP failures are tool results inside `200` responses, so read
outcomes from the results or from `fluent_prepare_total`, not from HTTP
statuses.

The first readiness measurement (50 clients × 10 calls, debug build, on one
laptop):

| Run | Resolver | Limits | Throughput | p50 / p95 / max | Outcomes |
| --- | --- | --- | --- | --- | --- |
| Gate | 200 ms | defaults, no store | 38.6/s | 1.26 / 1.39 / 1.45 s | 500 `ok` |
| Quota | 200 ms | defaults, `[store]` | — | 0.003 / 1.42 / 1.45 s | 200 `ok`, 300 `quota_exhausted` |
| Busy | 1 s | `max_concurrent_resolutions = 2` | 9.9/s | 5.01 / 5.03 / 5.22 s | 100 `ok`, 400 `resolver_unavailable` (busy) |

With eight slots and a 200 ms resolver the gate allows at most 40 calls a
second, and the server sustained 38.6: latency is queueing for slots. The
quota refused every call past the 200th in about a millisecond, without
reaching the resolver. With two slots and a 1 s resolver, calls that could
not start within the 5 second admission wait were turned away as busy.

## What is logged

`fluent serve` logs JSON lines to stderr. `RUST_LOG` sets the level (default
`info`).

Each tool call runs in a `tool_call` span and ends with one event, `tool call
succeeded` or `tool call failed`. They carry:

| Field | Content |
| --- | --- |
| `tool` | The tool name. |
| `sub_hash` | The first 12 hex digits of the SHA-256 of the principal's `sub`, when there is a principal. Enough to follow one user; not their identity. |
| `registration`, `tx` | The registration slug and transaction name, for transaction tools. |
| `arguments` | The argument **names**, comma-separated. |
| `outcome`, `code` | `ok` or the error code. |
| `duration_ms` | The call's duration, limits included. |

Never logged, at any level:

- argument values: addresses, amounts, datums or anything else a caller
  sends. The MCP library logs whole messages at `debug` and `trace`; the
  server drops those events whatever `RUST_LOG` says;
- bearer tokens, JWTs or their signatures: the `Authorization` header is
  removed once a request is authenticated;
- the TRP API key, or any other secret named by a `*_env` key;
- the principal's `sub` or email in the clear;
- the transcript events `fluent demo record` writes to its own file (see
  [the journey evidence guide](journey-evidence.md#record-the-session)):
  they carry redacted results, and the log filter drops them.

Errors returned to callers follow the same rule: their `details` hold
identifiers, argument names, limits and counts only.

Logged: the principal's `sub_hash`; registration slugs, transaction names
and networks; error codes and messages; resolver HTTP statuses; transaction
hashes of prepared transactions at `debug`; startup configuration without
secrets; and lifecycle events such as sessions starting and ending.
