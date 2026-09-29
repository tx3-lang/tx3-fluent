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

Every failed call counts against the quota except a quota rejection itself;
a call the gate turns away has already been counted.

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
  waited for a slot can still finish. Clients such as ChatGPT give up on
  tool calls after roughly a minute; stay below that.
- **`per_user_daily_quota`** — Compare `fluent_quota_rejections_total` with
  the number of active users. Rejections from a handful of principals point
  at automation or abuse; revoke with `fluent admin revoke` rather than
  lowering the quota for everyone. Widespread rejections mean the quota is
  too low for normal use. Each call costs one resolver request, so the quota
  times the number of users bounds the daily resolver load.

The limits are read at startup; change them in the file or with
`FLUENT_LIMITS__*` overrides and restart.

### First readiness measurement

A load run against a scripted resolver answering in 200 ms, with the default
limits except `max_concurrent_resolutions = 8`, is summarized in the pull
request that introduced these limits. Repeat it after changing the limits or
the deployment: start the server against a local resolver stub, open a
session, then send the same `tools/call` from many clients, for example with
[`oha`](https://github.com/hatoo/oha):

```sh
oha -c 50 -n 500 -m POST \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -H "authorization: Bearer $FLUENT_API_TOKEN" \
  -H "mcp-session-id: $SESSION" -H 'mcp-protocol-version: 2025-06-18' \
  -d @call.json http://127.0.0.1:8080/mcp
curl -s http://127.0.0.1:8080/metrics | grep '^fluent_'
```

`oha` reports HTTP latency; MCP failures are tool results inside `200`
responses, so read the outcomes from `fluent_prepare_total`.

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
- the principal's `sub` or email in the clear.

Errors returned to callers follow the same rule: their `details` hold
identifiers, argument names, limits and counts only.

Logged: the principal's `sub_hash`; registration slugs, transaction names
and networks; error codes and messages; resolver HTTP statuses; transaction
hashes of prepared transactions at `debug`; startup configuration without
secrets; and lifecycle events such as sessions starting and ending.
