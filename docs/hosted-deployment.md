# Hosted deployment

The public Fluent runs the same image operators self-host,
`ghcr.io/tx3-lang/tx3-fluent`, with
[`examples/config/hosted.toml`](../examples/config/hosted.toml): OIDC callers,
a SQLite store of users and selections, per-user quotas and the companion
site. This page lists what that deployment needs from its platform. How to
roll it out (manifests, secrets management, DNS and TLS) belongs to the
platform and is not covered here.

## Image

Deploy a release tag (`vX.Y.Z`) or an exact build (`sha-<commit>`), never
`latest`, so a restart cannot change the version. The image runs as uid 1000
and starts `fluent serve --http --config /data/fluent.toml`. On SIGTERM it
ends every MCP session and waits for requests in flight, then exits; give it
a termination grace period longer than `limits.global_cutoff_secs` (45 s by
default).

Run **one replica**. MCP sessions live in the process's memory, so a second
replica cannot serve a session the first one started, and the SQLite store
is a single file on one volume. A restart ends every session; clients
reconnect.

## Volume

| Path | Content | Persistence |
| --- | --- | --- |
| `/data/fluent.toml` | `hosted.toml`, for example from a config map mounted read-only. | Configuration. |
| `/data/registrations` | One registration bundle per subdirectory, plus `.cache/` with verified registry artifacts. | Persistent, or rebuilt on start from the reviewed bundles in [`deploy/hosted/registrations`](../deploy/hosted/registrations). |
| `/data/fluent.sqlite` | Users, their selections and daily quota counts, with its `-wal` and `-shm` files. | **Persistent.** Back it up; losing it resets every user's selections. |

Mount one persistent volume at `/data`, writable by uid 1000 (on Kubernetes,
`securityContext.fsGroup: 1000`), and the configuration over
`/data/fluent.toml`. The store's schema migrates at startup. Bundles are
loaded once at startup: publishing, changing or removing one takes a restart
(a rollout).

## Environment

`hosted.toml` names its secrets by variable; the platform supplies the values
from its secret store. None of them may appear in the configuration file or
the image.

| Variable | Named by | Holds |
| --- | --- | --- |
| `TRP_MAINNET_API_KEY` | `networks.mainnet.trp_api_key_env` | The mainnet TRP endpoint's API key. |
| `TRP_PREPROD_API_KEY` | `networks.preprod.trp_api_key_env` | The preprod TRP endpoint's API key. |
| `FLUENT_METRICS_TOKEN` | `server.metrics_token_env` | Bearer token Prometheus presents to `/metrics`. |
| `FLUENT_SESSION_SECRET` | `site.session_secret_env` | Site cookie signing key, at least 32 bytes (`openssl rand -hex 32`). Changing it signs every user out. |
| `FLUENT_OIDC_CLIENT_ID` | `site.oidc_client_id_env` | The site's OIDC client ID. |
| `FLUENT_OIDC_CLIENT_SECRET` | `site.oidc_client_secret_env` | The site's OIDC client secret. |

Also set `RUST_LOG` when a level other than `info` is wanted. Any
configuration key can be overridden per environment with a `FLUENT_*`
variable, for example `FLUENT_LIMITS__PER_USER_DAILY_QUOTA=100`; see
[Environment overrides](../README.md#environment-overrides). Check the result
before a rollout, in the image, with the same variables set:

```sh
fluent config check --config /data/fluent.toml
fluent registrations check --config /data/fluent.toml
```

`config check` prints the configuration with each secret shown as `"<set>"` or
`"<unset>"`; `serve --http` refuses to start while a named secret is unset.

## Network

The container listens on `0.0.0.0:8080` (`server.listen`). Terminate TLS in
front of it and route `public_url` (`https://fluent.tx3.land`) to that port.

- `/mcp` answers only requests whose `Host` is `public_url`'s host (or
  loopback), so the proxy must pass the original `Host` header.
- Route `/mcp`, `/.well-known/oauth-protected-resource`, `/healthz` and the
  site's pages (`/`, `/auth/*`, `/protocols`, `/connect`, `/site.css`).
  `/metrics` does not need to be public.
- The identity provider must allow the site's `redirect_url`
  (`{public_url}/auth/callback`) and issue access tokens for `audience`
  (`{public_url}/mcp`).
- Outbound, the server reaches the OIDC issuer and its JWKS, the TRP
  endpoints, and the registry for registry-sourced bundles.
- Streamable HTTP holds event streams open; give the proxy an idle timeout
  above `limits.global_cutoff_secs` and do not buffer responses.

## Health and metrics

| Endpoint | Authentication | Use |
| --- | --- | --- |
| `GET /healthz` | none | Liveness and readiness. Answers `200 {"status":"ok","version":"…"}` once the registrations are loaded and the listener is up. |
| `GET /metrics` | `Authorization: Bearer $FLUENT_METRICS_TOKEN` | Prometheus scrape. Only a direct loopback caller may omit the token, and a request with `Forwarded` or `X-Forwarded-For` always needs it. |

On Kubernetes, for example:

```yaml
readinessProbe:
  httpGet: { path: /healthz, port: 8080 }
livenessProbe:
  httpGet: { path: /healthz, port: 8080 }
  periodSeconds: 20
```

and a scrape job with `authorization: { credentials: <metrics token> }`.
Registry fetches happen before the listener starts, and each may take up to
20 seconds, so allow for them in the first readiness check.

The series, the queries worth alerting on and how to size `[limits]` from
them are in [the operations guide](operations.md), which also lists what the
JSON logs on stderr contain and never contain.

## Operations

- **Revoke a user**: `fluent admin revoke --config /data/fluent.toml --sub <sub>`
  in the running container (`kubectl exec`). They see no tools and every
  call fails as `unauthorized`, including in sessions already open.
- **Upgrade**: run `config check` and `registrations check` with the new
  image and the live configuration, back up `fluent.sqlite`, then roll out
  the new tag.
- **Roll back**: deploy the previous tag. A store migrated by a newer version
  may not open with an older one; restore the backup taken before the
  upgrade if it does not.
