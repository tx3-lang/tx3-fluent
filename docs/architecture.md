# Architecture

Tx3 Fluent turns published Tx3 protocols into MCP tools that prepare Cardano
transactions. Each transaction a protocol's TII defines becomes one tool. A
call resolves the transaction through a TRP endpoint and returns it unsigned,
with a summary decoded from its CBOR. A wallet signs and submits after the
user has reviewed it.

Four rules shape everything below:

- **No keys, no signing, no submission.** Nothing in Fluent can move funds.
  Every result says so in fields whose values are fixed by type.
- **Protocol behavior lives in artifacts, not code.** A protocol is added by
  adding a registration bundle (TII, skill, deployment profile), never a
  handler.
- **One engine for every distribution.** stdio, self-hosted HTTP and the
  hosted service run the same binary and the same preparation path; the
  parity test requires byte-identical replies across them.
- **Callers choose a tool and its arguments, nothing else.** They cannot name
  a resolver, send TIR, or override a value the deployment profile binds.

## Components

```text
  MCP client (desktop, ChatGPT, Inspector)              Browser
      │ stdio, or Streamable HTTP at /mcp                  │ /, /protocols, /connect
      ▼                                                    ▼
┌───────────────────────── fluent-server: the `fluent` binary ─────────────────────────┐
│ http::auth  none | token | oidc        site  sign-in (OIDC code + PKCE), selections  │
│ mcp         FluentHandler ── ToolScope ◄── store  SQLite: users, selections, quota   │
│ limits      Gate (concurrency, cutoff) + Quota (per principal, per UTC day)          │
│ metrics     Prometheus text at /metrics     logging  JSON lines on stderr            │
└───────────────────────────────┬──────────────────────────────────────────────────────┘
                                ▼
┌───────────────────────── fluent-core: library ───────────────────────────────────────┐
│ config        TOML file + FLUENT_* overrides; `*_env` keys name secrets              │
│ registration  bundle loader ──► registry::OciFetcher ──► OCI registry (by digest)    │
│ catalog       TII transaction ──► MCP tool descriptor (self-contained JSON Schema)   │
│ engine        validate arguments ──► tx3-sdk client ──► TRP endpoint                 │
│ summary       decode the returned CBOR on its own; check the transaction hash        │
│ address       offline address inspection                                             │
│ envelope, error   the PreparedTransaction result and the stable error codes          │
└──────────────────────────────────────────────────────────────────────────────────────┘

  xtask (never shipped): `verify` decodes a transaction a third time, `transcript` renders logs
```

| Crate | Owns | Does not own |
| --- | --- | --- |
| `fluent-core` | Configuration, registrations, the tool catalog, preparation, summaries, address inspection, results and errors. Its only I/O is the registry fetch and the TRP call. | Transports, authentication, users, limits. |
| `fluent-server` | The `fluent` CLI, the MCP handler, stdio and Streamable HTTP, authentication, the store, limits, metrics, logging and the companion site. | Anything protocol-specific. |
| `xtask` | Evidence tooling run as `cargo xtask`: independent decode checks and log transcripts. | Anything the service runs; it is never built into the image or released. |

## Registrations

A registration binds one protocol's TII, its consumption skill (`SKILL.md`)
and one deployment profile of that TII. The profile picks the network
(`mainnet`, `preprod` or `preview`) and supplies the parties and environment
values the deployment fixes, such as a script address.

- **Identity.** Each registration has a `slug`, unique on the server. Its
  revision is the first 12 hex digits of
  `sha256(tii_digest ‖ skill_digest ‖ profile ‖ network)`, so changing any of
  the four makes a new revision.
- **Sources.** A local bundle carries its TII file. A registry bundle names
  an OCI reference and pins the manifest digest and the TII digest; at
  startup the server fetches the artifact anonymously and accepts it only
  when every digest matches. Verified copies are cached under `.cache/` by
  manifest digest.
- **Validation at load.** The loader rejects a bundle whose files do not
  parse, whose TII does not match its manifest, whose profile is missing or
  not a public network, or whose skill is bound to another protocol, TII or
  network. A rejected bundle is logged and skipped; the others still load.
- **Loaded once.** Bundles are read at startup. Adding, changing or removing
  one takes a restart.

The [README](../README.md#registration-bundles) has the file formats and the
complete rules; the [skill template](skill-template.md) fixes the structure
of every skill.

## From TII to tools

At startup, the catalog builds one tool descriptor per TII transaction of
every loaded registration, named `{slug}_{tx}`, plus two fixed tools:
`fluent_get_skill` and `fluent_inspect_address`. Each transaction tool's input
schema is the transaction's parameters with every `$ref` inlined, followed by
the parties and environment values the profile leaves open. Values the
profile binds are left out, and `additionalProperties` is `false`. The output
schema is the `PreparedTransaction` schema.

The catalog is computed once and never changes while the server runs. When
any loaded registration's tools cannot be built (an unresolvable or recursive
`$ref`, a name too long or shared), the server refuses to start, unlike a
bundle that fails to load, which is only skipped.

## Serving a request

**`tools/list`** returns the catalog filtered by the caller's `ToolScope`:
the registrations this caller may see. The scope is evaluated on every
request, so a revocation or a changed selection applies at once, including
to a session that is already open.

**`tools/call`** on a transaction tool:

1. Checks the tool's registration is in the caller's scope
   (`registration_unavailable` otherwise).
2. Applies the limits. The whole call must finish within
   `global_cutoff_secs`. Inside that window, the caller's daily quota is
   charged first (only with a store), then the call waits up to 5 seconds
   for one of `max_concurrent_resolutions` slots.
3. `Engine::prepare` rejects arguments that try to override a value the
   profile binds, validates the rest against the tool's input schema, binds
   the parties and resolves through the Rust SDK against the network's TRP
   endpoint, bounded by `resolver_timeout_secs`.
4. `summary::decode` reads the returned CBOR without the arguments, so the
   summary is an independent account of what the resolver built. If the
   hash of the body bytes differs from the resolver's hash, the call fails as
   `internal`.
5. The result is a `PreparedTransaction`: the unsigned CBOR, its hash, the
   protocol and registration revision, the network, the summary, and fixed
   `signed: false`, `submitted: false` and `next_steps` values.

`fluent_get_skill` returns the skill of a registration in scope, and
`fluent_inspect_address` decodes an address offline. A failure is a tool
result with `isError: true` carrying a stable `code`, a message and optional
details. Only an unknown tool name is a JSON-RPC error.

## Distributions and tenancy

| Distribution | Transport | Authentication | Who sees what |
| --- | --- | --- | --- |
| Desktop | `serve --stdio` | none: whoever starts the process | every registration |
| Self-hosted | `serve --http`, the container | `token` (or `none`, loopback only) | every registration |
| Hosted | `serve --http` with `[store]` and `[site]` | `oidc` | each user, the registrations they selected |

The `Scoping` trait chooses the scope: `AllRegistrations` everywhere except
`oidc` with a store, where `UserScopes` reads the user's selections. A
selection records the registration's revision when it was made; when the
registration changes revision, the selection shows "update required" on the
site and its tools disappear until the user selects it again. A revoked user
sees no tools, and every call fails as `unauthorized`.

## Transport, authentication and sessions

Over HTTP, an axum middleware authenticates every request to `/mcp` before
the MCP library sees it, attaches the caller's `Principal` (the token's
`sub`, and `email` when present) and drops the `Authorization` header.
`oidc` checks a JWT against the issuer's JWKS (cached for ten minutes),
`token` compares a static bearer token in constant time, and `none` refuses
to bind anything but loopback. A rejected request gets `401` with the RFC
9728 metadata URL, which is how ChatGPT discovers the issuer.

Requests must also name an allowed `Host`, to guard against DNS rebinding,
and stay under 256 KiB.

Sessions depend on the client's MCP protocol version:

- **Before `2026-07-28`**, a client opens a session with `initialize`. The
  session lives in the process's memory and is bound to the principal that
  opened it; a request on it authenticated as anyone else is refused. Open
  sessions of a user receive `notifications/tools/list_changed` when the
  user changes a selection.
- **From `2026-07-28`**, a client sends no `initialize` and holds no session.
  Each request gets its own handler, bound to the principal of its own
  token.

Nothing else depends on a session: quotas are counted per principal in the
store, and scopes are read per request.

## State

| State | Where | Lifetime |
| --- | --- | --- |
| Configuration | One TOML file plus `FLUENT_*` overrides | Read at startup |
| Secrets | Environment variables named by `*_env` keys | Read at startup; never logged or serialized |
| Registration bundles | `[registrations].dir` | Read at startup |
| Registry artifacts | `.cache/` in the bundle directory, by manifest digest | Rebuilt from the registry when missing or unverifiable |
| Users, selections, quota counts | SQLite file at `[store].sqlite_path` | Persistent; migrated at startup |
| MCP sessions | Process memory | Until the client ends them or the process restarts |
| JWKS, OIDC discovery | Process memory | Ten minutes for the JWKS; discovery until restart |
| Site sessions | Signed cookies in the browser | Seven days |

## Trust boundaries

- **Keys.** Fluent never receives a key, a seed phrase or a signature, and
  has no code path that signs or submits.
- **Callers.** A caller picks a tool and its arguments. The resolver URL,
  its API key and the profile's bound values come from the operator.
- **Registry artifacts.** Bytes are accepted only when they match the pinned
  digests. The recorded provenance (registry, reference, source digest and
  revision) says where the bytes came from, not who published them.
- **Skills.** A skill is text the assistant reads as instructions. It
  installs nothing and runs nothing. Hosted skills are reviewed when they
  are merged.
- **Results.** The summary is decoded from the transaction bytes alone, and
  `cargo xtask verify` checks those bytes a third time, independently of
  Fluent's decoder. The summary shows what the bytes would do if signed now;
  the wallet review remains the user's.
- **Logs.** Logs never contain argument values, results, tokens or API keys,
  and name callers only by a hash of their subject. See
  [what is logged](operations.md#what-is-logged).

## Known limitations

- **One replica.** Sessions live in memory and the store is one SQLite file
  on one volume, so the server runs as a single process. A restart or an
  eviction ends every session, and clients reconnect.
- **Static catalog.** New or changed bundles take effect only on restart.
- **One network per registration.** The operator fixes the deployment
  profile in each bundle, so a protocol offered on two networks needs two
  bundles and two skills, and users choose registrations, not networks.
- **Skills live beside the artifact.** A registry bundle pins its TII by
  digest but carries its skill as a separate file in the bundle, not in the
  published artifact.
- **Selections follow revisions.** Any change to a registration's TII or
  skill asks every user who selected it to select it again.
- **Tool-list refresh in ChatGPT.** ChatGPT does not act on
  `tools/list_changed`; after a selection or bundle change, the user
  refreshes the connector.
- **Provenance is not identity.** Registry provenance is not a verified
  publisher identity.
