# Tx3 Fluent

Tx3 Fluent turns published Tx3 protocols into tools that agents and people can
call to prepare Cardano transactions. It resolves each request through a TRP
endpoint and returns an **unsigned, unsubmitted** transaction with a readable
summary. Fluent never holds keys, signs or submits: a wallet does that after the
user has reviewed the transaction.

This repository is at an early stage. It holds the workspace layout, the
contracts shared by every later component (the configuration model, the error
type and the result envelope), offline address inspection, the registration
bundle loader with its registry artifact fetch, the tool catalog and the
transaction preparation engine, and the MCP server over stdio and over
Streamable HTTP with bearer-token or OAuth 2.1 (OIDC) authentication, and the
hosted SQLite store of users and their registration selections that scopes each
OIDC user's tools (`fluent admin` revokes a user or prints their selections).
Transaction tools run under public-service limits (a daily quota per user, a
server-wide concurrency gate and a global cutoff), and the HTTP server exposes
Prometheus metrics at `/metrics`; see [the operations guide](docs/operations.md).
The site is added later.

## Layout

| Path | Contents |
| --- | --- |
| `crates/fluent-core` | Library: configuration, errors, result envelopes, address inspection, registration bundles, tool catalog, preparation engine and transaction summaries. |
| `crates/fluent-server` | The `fluent` binary: CLI, the MCP server over stdio and HTTP, and the hosted selection store (migrations in `migrations/`); later the site. |
| `examples/config` | Example configurations, loaded by the tests. |
| `docs/operations.md` | Operating a server: metrics, setting limits from them, and what is logged. |
| `crates/fluent-core/tests/fixtures/registrations` | Valid and invalid registration bundles, loaded by the tests. |
| `crates/fluent-core/tests/fixtures/tii` | TII files used without a registration, such as the SDK spec's `complex.tii`. |
| `crates/fluent-core/tests/golden` | Reviewed tool descriptor lists the catalog tests compare against. |
| `crates/fluent-core/tests/fixtures/tx` | Real preprod transactions (CBOR hex) with their expected summaries, checked against the chain explorer. |

## Build and test

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.94 with `rustfmt` and
`clippy`); `rustup` installs it on first use.

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

CI runs the last three on every pull request and push to `main`; each job
blocks.

`crates/fluent-core/tests/live_preprod.rs` prepares a transfer against a live
preprod resolver. It skips green unless `FLUENT_TRP_URL_PREPROD` is set; then
it needs `TEST_PARTY_A_ADDRESS` (a funded sender) and `TEST_PARTY_B_ADDRESS`
(the receiver), and sends `FLUENT_TRP_API_KEY_PREPROD` as the API key when it
is set. It only resolves: nothing is signed or submitted.

```sh
FLUENT_TRP_URL_PREPROD=https://cardano-preprod.trp-m1.demeter.run \
FLUENT_TRP_API_KEY_PREPROD=… TEST_PARTY_A_ADDRESS=addr_test1… TEST_PARTY_B_ADDRESS=addr_test1… \
cargo test -p fluent-core --test live_preprod -- --nocapture
```

The CI job `live preprod` runs it with the organization secrets of the same
names, against the endpoint above.

## Usage

```sh
fluent --version
fluent config check --config examples/config/self-hosted.toml
```

`config check` loads the file, applies environment overrides, reads the
secrets it names, validates the result and prints it with secrets redacted. It
exits non-zero and prints the problem when the configuration is invalid.

```sh
fluent registrations check --config examples/config/self-hosted.toml
```

`registrations check` loads the configuration, then every bundle in
`[registrations].dir`, fetching [registry artifacts](#registry-artifacts), and
prints one TOML `[[registration]]` table per registration (slug, protocol,
network, profile, revision, digests, bundle) and one `[[rejected]]` table per
rejected bundle (bundle, error code, message). A registry-sourced registration
adds a `[registration.provenance]` table: `registry_url`, `ref`,
`manifest_digest`, and, when the manifest has them, `source_digest` and
`source_revision`. It exits non-zero when any bundle is rejected or the
directory cannot be read.

```sh
fluent prepare --config fluent.toml \
  --registration transfer_preprod --tx transfer \
  --args '{"quantity": 2000000, "sender": "addr_test1…", "receiver": "addr_test1…", "middleman": "addr_test1…"}'
```

`prepare` loads the configuration and the registrations, prepares one
transaction (see [Preparing transactions](#preparing-transactions)) and prints
the `PreparedTransaction` envelope as JSON. On failure it prints
`{"error": {"code", "message", "details"}}` instead (`details` only when there
are some) and exits non-zero. Rejected bundles are reported on stderr and do
not stop the others. Nothing is signed or submitted.

```sh
fluent address inspect addr_test1vz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerspjrlsz
```

`address inspect` decodes one Cardano address (bech32 or hex; base58 for
Byron) without contacting the chain and prints its kind, network and
credentials as JSON:

```json
{
  "input": "addr_test1vz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerspjrlsz",
  "kind": "shelley_enterprise",
  "network": "preprod_or_preview",
  "network_id": 0,
  "payment_credential": {
    "type": "key_hash",
    "hash_hex": "9493315cd92eb5d8c4304e67b7e16ae36d61d34502694657811a2c8e"
  },
  "stake_credential": null,
  "bech32": "addr_test1vz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerspjrlsz",
  "hex": "609493315cd92eb5d8c4304e67b7e16ae36d61d34502694657811a2c8e",
  "notes": [
    "Testnet addresses do not distinguish preprod from preview; confirm the network with the user."
  ]
}
```

`kind` is `shelley_base`, `shelley_enterprise`, `shelley_pointer`, `stake` or
`byron`; `network` is `mainnet`, `preprod_or_preview` or `unknown`. Credential
`type` is `key_hash` or `script_hash`, and a stake credential may instead be a
`pointer` with `hash_hex: null` and its `slot`, `tx_idx` and `cert_idx`.
Missing credentials are `null`. Byron addresses carry no credentials; their
`network` comes from the address's network tag when present. `notes` states
what the address cannot tell, such as which testnet it belongs to. Malformed
input exits non-zero with an `invalid arguments` error.

## Serving MCP

```sh
fluent serve --stdio --config fluent.toml
```

`serve --stdio` loads the configuration and the registrations, then speaks MCP
on stdin and stdout until the client closes stdin. Logs go to stderr as JSON
lines (`RUST_LOG` filters them; the default is `info`, and the MCP library's
`debug` and `trace` events, which carry message bodies, are always dropped);
stdout carries only MCP messages. Rejected bundles are logged and do not stop the others; a
catalog whose tools cannot be built (see [Tool catalog](#tool-catalog)) stops
the server at startup.

Every session sees every loaded registration. The server lists the
[tool catalog](#tool-catalog) and calls a tool by name: a transaction tool
prepares its transaction (see [Preparing transactions](#preparing-transactions)),
`fluent_get_skill` returns the registration's skill (a `scope/name` must match
exactly one registration, otherwise name its slug), and `fluent_inspect_address`
inspects the address. A result carries the JSON both as `structuredContent` and
as one text content. A failure is a result with `isError: true` whose text is
`{"error": {"code", "message", "details"}}`, never a JSON-RPC error; only an
unknown tool name is. The server reports itself as `tx3-fluent` with the
crate version, offers the `tools` capability with `listChanged`, and gives the
client short instructions: call `fluent_get_skill` first, nothing is signed or
submitted.

To drive it from [MCP Inspector](https://github.com/modelcontextprotocol/inspector)
(2.x), put the server command after `--`, so Inspector does not take
`--config` as its own option:

```sh
npx @modelcontextprotocol/inspector -- fluent serve --stdio --config fluent.toml
```

Inspector's `--cli` mode reads it the other way round: the server command
first, then `--` and Inspector's options:

```sh
npx @modelcontextprotocol/inspector --cli fluent serve --stdio --config fluent.toml \
  -- --method tools/call --tool-name fluent_get_skill --tool-arg protocol=transfer_preprod
```

A desktop client registers the same command, for example:

```json
{
  "mcpServers": {
    "tx3-fluent": {
      "command": "fluent",
      "args": ["serve", "--stdio", "--config", "/path/to/fluent.toml"]
    }
  }
}
```

### Over HTTP

```sh
fluent serve --http --config fluent.toml
```

`serve --http` serves the same MCP server over
[Streamable HTTP](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#streamable-http)
on `[server].listen`, until SIGTERM or Ctrl-C; then it ends every session and
waits for requests in flight. Sessions are kept in memory, so a restart ends
them.

| Route | Authenticated | Serves |
| --- | --- | --- |
| `/mcp` | yes | MCP: `POST` messages, `GET` the session's event stream, `DELETE` the session. |
| `GET /healthz` | no | `{"status": "ok", "version": "<crate version>"}`. |
| `GET /.well-known/oauth-protected-resource` | no | [RFC 9728](https://www.rfc-editor.org/rfc/rfc9728) metadata, `oidc` mode only; also under `…/mcp`. |
| `GET /metrics` | loopback or token | Prometheus metrics; see [the operations guide](docs/operations.md#metrics). |

Every request to `/mcp` is authenticated as [`[auth]`](#auth-required) says:

- `none`: every caller is accepted. Fluent refuses to start unless `listen`
  is a loopback address.
- `token`: `Authorization: Bearer <token>` must equal the value of
  `token_env`, compared in constant time. Fluent refuses to start when that
  variable is unset or empty.
- `oidc`: the bearer token must be a JWT signed with RS256 or ES256 by a key,
  named by its `kid`, from the issuer's JWKS at `jwks_url`; its `iss` must be
  `issuer`, its `aud` must be or contain `audience`, it must carry `sub`, and
  `exp` (required) and `nbf` (when present) must hold, give or take a
  minute. Fluent caches the JWKS for 10 minutes, and fetches it again for an
  unknown `kid`, at most once a second. `server.public_url` is required.

A rejected request gets `401` with an empty body and
`WWW-Authenticate: Bearer resource_metadata="{public_url}/.well-known/oauth-protected-resource"`
(just `Bearer` when there is no `public_url`). In `oidc` mode the metadata
reads:

```json
{
  "resource": "{public_url}/mcp",
  "authorization_servers": ["{issuer}"],
  "bearer_methods_supported": ["header"],
  "scopes_supported": []
}
```

The caller's `sub`, and `email` when the token has one, form its principal
(`sub` is `token` in `token` mode). A session belongs to the principal that
initialized it; a request on that session authenticated as anyone else is
refused. Fluent drops the `Authorization` header once the request is
authenticated and never logs a token. Every session still sees every loaded
registration.

A request body may be at most 256 KiB (`413` otherwise). To guard against DNS
rebinding, `/mcp` only answers a `Host` naming `localhost`, a loopback
address, the listen address or `public_url`'s host (`403` otherwise), so a
public deployment behind a proxy needs `public_url`.

Transaction tool calls run under [`[limits]`](#limits-optional): each must
finish within `global_cutoff_secs` (`resolver_timeout` otherwise), at most
`max_concurrent_resolutions` prepare at once (a call that waits 5 seconds for
a slot fails as `resolver_unavailable` with `details.reason =
"server_busy"`), and, when a `[store]` is configured, each principal may make
`per_user_daily_quota` of them per UTC day (then `quota_exhausted`, with
`details.resets_at` the next UTC midnight). Skill and address tools are not
limited. See [the operations guide](docs/operations.md) for the metrics and
how to set the limits.

`GET /metrics` answers a direct loopback caller, or any caller presenting
`Authorization: Bearer` with the token in `server.metrics_token_env` or, in
`token` mode, the API token (`401` otherwise). A request carrying `Forwarded`
or `X-Forwarded-For` came through a proxy and needs a token.

To try it with MCP Inspector in `token` mode:

```sh
FLUENT_API_TOKEN=… fluent serve --http --config examples/config/self-hosted.toml
npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8080/mcp \
  --transport http --header "Authorization: Bearer $FLUENT_API_TOKEN" --method tools/list
```

### Connecting ChatGPT

A hosted deployment such as [`examples/config/hosted.toml`](examples/config/hosted.toml)
runs `fluent serve --http` in `oidc` mode behind TLS at `public_url`. The
identity provider (the `issuer`) must issue access tokens for `audience` —
the MCP endpoint, `{public_url}/mcp` — and let ChatGPT register itself as a
client. Then:

1. In ChatGPT, turn on developer mode (Settings → Apps & Connectors →
   Advanced settings).
2. Create a connector and give it the MCP server URL, ending in `/mcp`, for
   example `https://fluent.tx3.land/mcp`, with OAuth authentication.
3. ChatGPT reads the `401` challenge and the protected resource metadata,
   finds the issuer, and sends you through its sign-in. Complete it.
4. After the server's tools change (new or updated registrations), refresh the
   connector in ChatGPT's settings so it lists them again.

See OpenAI's [Connect from ChatGPT](https://developers.openai.com/plugins/deploy/connect-chatgpt)
and the [MCP authorization specification](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization).

## Configuration reference

Fluent reads one TOML file, passed with `--config`. **Unknown keys are errors.**
Keys ending in `_env` hold the *name* of an environment variable, never a
secret. Fluent reads the variable's value at startup, keeps it in memory, and
never serializes it. An empty value counts as unset. Redacted output shows
`"<set>"` or `"<unset>"` in place of these keys.

### `[server]` (optional)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | socket address | `"127.0.0.1:8080"` | Address and port `serve --http` listens on. |
| `public_url` | URL | none | Externally visible base URL, advertised in the OAuth metadata. Required in `oidc` mode. |
| `metrics_token_env` | variable name | none | Variable holding a bearer token that may read `GET /metrics` from any address. `serve --http` refuses to start when it is set and the variable is unset or empty. |

### `[registrations]` (required)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `dir` | path | required | Directory holding one [registration bundle](#registration-bundles) per subdirectory. A relative path resolves against the working directory. |

### `[networks.<name>]` (at least one)

One table per network, for example `[networks.mainnet]`. Names use lowercase
letters, digits, `_` or `-`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `trp_url` | URL | required | TRP endpoint. Must not embed credentials. |
| `trp_api_key_env` | variable name | none | Variable holding the TRP API key, when the endpoint needs one. |

### `[limits]` (optional)

All values must be greater than zero.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `resolver_timeout_secs` | integer | `30` | Seconds one resolver call may take. Must not exceed `global_cutoff_secs`. |
| `global_cutoff_secs` | integer | `45` | Seconds one transaction tool call may take end to end, waiting for a slot included. |
| `max_concurrent_resolutions` | integer | `8` | Transaction preparations in flight at once, server-wide. A call waits at most 5 seconds for a slot. |
| `per_user_daily_quota` | integer | `200` | Transaction tool calls one principal may make per UTC day. Enforced by `serve --http` with a `[store]`. |

### `[auth]` (required)

`mode` selects how callers authenticate, and only that mode's keys are allowed.

| `mode` | Keys | Meaning |
| --- | --- | --- |
| `"none"` | none | Every caller is accepted; `serve --http` only binds loopback addresses. |
| `"token"` | `token_env` (variable name, required) | Callers present one static bearer token. |
| `"oidc"` | `issuer` (URL), `jwks_url` (URL), `audience` (string), all required | Callers present a JWT from an OpenID Connect issuer. Requires `server.public_url`. |

See [Over HTTP](#over-http) for how each mode checks a request.

### `[store]` (optional)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `sqlite_path` | path | required | SQLite database file: users, their selections and daily quota counts. |

### `[site]` (optional)

When `enabled = true`, every other key is required.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `enabled` | boolean | `false` | Whether the browser site is served. |
| `session_secret_env` | variable name | none | Variable holding the session cookie signing key. |
| `oidc_client_id_env` | variable name | none | Variable holding the OIDC client ID used for sign-in. |
| `oidc_client_secret_env` | variable name | none | Variable holding the OIDC client secret used for sign-in. |
| `redirect_url` | URL | none | OIDC redirect URL registered with the identity provider. |

### Environment overrides

Any key can be set or overridden from the environment. Variable names use the
`FLUENT_` prefix, and double underscores separate the path segments:

| Variable | Sets |
| --- | --- |
| `FLUENT_SERVER__LISTEN=0.0.0.0:8080` | `server.listen` |
| `FLUENT_LIMITS__PER_USER_DAILY_QUOTA=50` | `limits.per_user_daily_quota` |
| `FLUENT_NETWORKS__PREPROD__TRP_URL=https://…` | `networks.preprod.trp_url` |
| `FLUENT_AUTH__MODE=none` | `auth.mode` |
| `FLUENT_SITE__ENABLED=true` | `site.enabled` |

Segments are matched in lowercase. Overrides take precedence over the file and
can supply keys the file omits. A `FLUENT_` variable containing `__` must name a
known key, or loading fails. `FLUENT_` variables without `__` are not
overrides, so they can hold secrets named by `*_env` keys.

### Examples

- [`examples/config/self-hosted.toml`](examples/config/self-hosted.toml):
  a local operator with a bearer token, a devnet and preprod.
- [`examples/config/hosted.toml`](examples/config/hosted.toml): a public
  deployment at `https://fluent.tx3.land` with OIDC from the Tx3 Auth0 tenant,
  mainnet and preprod, and the site enabled.

## Registration bundles

A registration binds one protocol's TII, its consumption skill and one
deployment profile. Each non-hidden subdirectory of `[registrations].dir` is
one bundle; entries whose names start with `.` (such as a cache) and plain
files are skipped. Registrations are loaded once at startup: changing a bundle
takes effect on restart.

```text
registrations/
  strike_staking_mainnet/
    registration.toml   manifest
    SKILL.md            consumption skill
    protocol.tii        TII; source = "local" only
  .cache/               registry artifacts, by manifest digest
```

### `registration.toml`

Unknown keys are errors. Paths are relative to the bundle and must stay inside
it.

```toml
slug = "strike_staking_mainnet"          # ^[a-z][a-z0-9_]{2,40}$, unique
[protocol]                                # equal to the TII protocol block
scope = "open-tx3"
name = "strike-staking"
version = "0.2.0"
source = "local"                          # or "registry"
[protocol.registry]                       # required iff source = "registry"
url = "https://oci.tx3.land"
ref = "open-tx3/strike-staking:0.2.0"
manifest_digest = "sha256:…"              # artifact manifest content digest
[artifact]
tii = "protocol.tii"                      # local only; default protocol.tii
tii_digest = "sha256:…"                   # required for registry; checked if set
[skill]
path = "SKILL.md"                         # default SKILL.md
[deployment]
profile = "mainnet"                       # mainnet | preprod | preview
network = "mainnet"                       # must be the profile's network
```

A registry-sourced bundle carries no TII: its TII comes only from the
digest-verified [registry fetch](#registry-artifacts).

### Registry artifacts

At startup, each `source = "registry"` bundle's TII is fetched anonymously
from `protocol.registry.url` by `ref`, for example
`oci.tx3.land/open-tx3/strike-staking:0.2.0`. The URL names only a scheme, a
host and an optional port. The fetch:

1. requires the SHA-256 of the returned manifest bytes to equal
   `manifest_digest`; otherwise the registration is rejected with "registry
   content changed", because the reference now names other content;
2. takes the manifest's one `application/tii+json` layer, whose digest must be
   SHA-256 and whose declared size must be at most 8 MiB;
3. pulls that layer's blob, which must not exceed the declared size, and
   requires its SHA-256 to equal the layer digest;
4. records the `application/tx3` layer digest (`source_digest`) and the
   `org.opencontainers.image.revision` manifest annotation
   (`source_revision`), when present, as the registration's provenance.

The TII's digest must then equal `artifact.tii_digest`, and every rule below
applies as it does to a local TII. Provenance records where the bytes came
from; it is not a verified publisher identity.

The verified manifest and TII are cached in `[registrations].dir/.cache/` as
`{manifest_digest}.manifest.json` and `{manifest_digest}.tii`. When both are
present, the manifest hashes to `manifest_digest` and the TII hashes to its
layer digest, the cached copy is used and no request is made. Cached files
that do not verify are ignored and replaced by a fresh fetch. Failing to write
the cache is logged and does not reject the registration.

Each artifact fetch may take 20 seconds. A fetch that fails or times out
rejects that registration only; the others still load, and every rejection is
logged at `warn` level.

### `SKILL.md`

YAML frontmatter between two `---` lines, then the Markdown body, which is
kept verbatim. Unknown frontmatter keys are errors.

```yaml
---
name: strike-staking
description: One sentence ending with when to use the skill.
license: Apache-2.0                       # optional
protocol: open-tx3/strike-staking:0.2.0   # scope/name:version
tii_digest: sha256:…                      # digest of the registration's TII
network: mainnet
revision: 1                               # the skill's own revision
dependencies:                             # optional
  - id: strike_balance
    description: What the assistant must obtain elsewhere.
    required_for: [stake]                 # transactions of the TII
---
```

### Rules

A bundle is rejected, without stopping the others, when:

- `registration.toml`, the TII or the skill cannot be read or parsed; the TII
  must parse as TII (`tx3_sdk::tii::Protocol` and `tii::spec::TiiFile`);
- a registry artifact cannot be fetched or verified, as described in
  [Registry artifacts](#registry-artifacts);
- the manifest breaks a rule above, or its `tii_digest` differs from the TII's;
- `[protocol]` differs from the TII `protocol` block;
- the profile is not defined in the TII, or is not `mainnet`, `preprod` or
  `preview` (so `local` is rejected);
- `deployment.network` differs from the profile's network
  (`network_mismatch`);
- the skill's `protocol`, `tii_digest` or `network` differs from the
  registration, or a dependency names a transaction the TII does not define
  ("incompatible skill binding");
- another loaded bundle has the same slug: every claimant is rejected.

Every rejection except `network_mismatch` has the code
`registration_unavailable`, and its message names the bundle path and the
broken rule.

### Digests and revision

`tii_digest` and `skill_digest` are `sha256:<hex>` over the exact file bytes.
The revision is the first 12 hex digits of the SHA-256 of the string
`{tii_digest}{skill_digest}{profile}{network}`, digests in their `sha256:<hex>`
form:

```sh
printf '%s' "$tii_digest$skill_digest$profile$network" | sha256sum | cut -c1-12
```

## Tool catalog

Every loaded registration offers one MCP tool per TII transaction. Two fixed
tools are always offered as well. Tool descriptors are computed once from the
loaded registrations; nothing here invokes a tool.

| Field | Transaction tool |
| --- | --- |
| name | `{slug}_{tx}`: the TII transaction name lowercased, every character outside `[a-z0-9_]` replaced by `_`; at most 64 characters. |
| title | `{protocol name} {version} · {tx} ({network})`, with the transaction name as the TII writes it. |
| description | The TII transaction description, if any, then: `Prepares an UNSIGNED {network} transaction for {scope}/{name}:{version}. Nothing is signed or submitted. Call fluent_get_skill for this protocol before using this tool.` |
| input schema | Self-contained; see below. |
| output schema | The `PreparedTransaction` schema, with every subschema inlined. |
| annotations | `readOnlyHint: true`, `destructiveHint: false`, `idempotentHint: false`, `openWorldHint: true`. |

The input schema starts from the transaction's `params` and has no `$ref`:

- `https://tx3.land/specs/v1beta0/tii#/$defs/{Name}` and the legacy
  `https://tx3.land/specs/v1beta0/core#{Name}` are replaced by the TII
  specification's definitions: `Address` is a string described as a bech32
  address, `Bytes` a hex string with an optional `0x`, `UtxoRef` a string
  matching `^(0x)?[0-9a-fA-F]{64}#[0-9]+$`, `AnyAsset` an object with
  `policy`, `asset_name` and `amount`, and `Utxo` an object;
- `#/components/schemas/{Name}` is replaced by the TII's own component;
- keywords written next to a `$ref`, such as a `description`, are kept;
- after the parameters come one required string property per party the
  deployment profile does not bind (named as the party in lowercase and
  described as its bech32 address), then every environment field the profile
  does not bind, required when the TII environment requires it;
- values the profile binds are left out entirely, and
  `additionalProperties` is `false`.

Every other keyword of the TII schemas (`description`, `minimum`, `pattern`,
`enum`, …) and the order of `required` are kept. A registration's tools cannot
be built, and it is reported as `registration_unavailable`, when a `$ref`
cannot be resolved or is recursive, when a tool name is too long or shared by
two of its transactions, or when a parameter, party and environment field
share an argument name (names are compared in lowercase, bound or not). Two
tools with the same name, across registrations or with a fixed tool, are an
error for the whole catalog.

| Fixed tool | Input | Output | Annotations |
| --- | --- | --- | --- |
| `fluent_get_skill` | `{ protocol }`: a registration slug or `scope/name` | `{ protocol: { scope, name, version, registration_slug, registration_revision, network }, skill_revision, dependencies, markdown }` | read-only, idempotent, closed world |
| `fluent_inspect_address` | `{ address }` | an `AddressReport` object | read-only, idempotent, closed world |

## Preparing transactions

`fluent_core::Engine` is built once at startup from the configuration and the
loaded registrations. For each registration it builds one Rust SDK client
(`Protocol::client()`) against the TRP endpoint of the registration's network
(`[networks.<network>].trp_url`), with the `dmtr-api-key` header when that
network's `trp_api_key_env` is set, and with the deployment profile selected.
A registration whose network has no `[networks]` table is kept and answered
with `network_mismatch`.

`Engine::prepare(PrepareRequest { registration, tx, args })`:

1. Finds the registration by slug (`unknown_protocol`) and the transaction by
   its TII name (`unknown_transaction`).
2. Rejects any argument named like a party or environment field the profile
   binds, compared in lowercase (`invalid_arguments`, "deployment-bound value
   cannot be overridden").
3. Validates the arguments against the transaction's tool
   [input schema](#tool-catalog) with the `jsonschema` crate, so unknown keys,
   missing required arguments, wrong types and broken patterns are rejected
   (`invalid_arguments`). `details.violations` lists `{ path, message }` per
   violation, where `path` is a JSON Pointer into the arguments and the
   message never quotes the value.
4. Binds each party argument with `Party::address`, passes the rest as
   arguments, and resolves. The SDK adds the profile's environment values and
   encodes every argument (`tii::encode`). The call is bounded by
   `limits.resolver_timeout_secs` (`resolver_timeout`).
5. Decodes the returned CBOR into the summary (below) and compares the
   resolver's transaction hash with the Blake2b-256 of the body bytes. A
   difference is `internal`, with both hashes in `details`.

Resolver errors are classified as follows. Details never carry an address,
an amount, a UTxO or an argument value.

| TRP / SDK error | Code | Details |
| --- | --- | --- |
| `InputNotResolved` whose query has a `min_amount` | `insufficient_funds` | `input`; `query.has_address`, `query.refs` (count); `search_space.matched` (count) |
| `InputNotResolved` without one | `input_not_resolved` | as above |
| `TxScriptFailure` | `script_failure` | `logs`: the first 20 lines |
| `MissingTxArg` | `invalid_arguments` | the argument name |
| `UnsupportedTir` | `registration_unavailable` | |
| `NetworkError`, `HttpError` | `resolver_unavailable` | `network`; `status` when the resolver answered |
| timeout | `resolver_timeout` | `timeout_secs` |
| unknown transaction | `unknown_transaction` | |
| `InvalidTirEnvelope`, `InvalidTirBytes`, `UnsupportedTxEra` | `registration_unavailable` | |
| `DeserializationError`, other JSON-RPC errors, `UnknownError`, `UnsupportedEra` | `resolver_unavailable` | `network` |
| an argument the SDK cannot encode, an unknown party | `invalid_arguments` | |

`fluent_core::summary::decode(tx_hex)` is a pure function over the CBOR alone,
never the arguments, so the summary is an independent check of what the
resolver built. It reads one complete Conway-era transaction and returns its
hash (Blake2b-256 of the body bytes) and a `TransactionSummary`: inputs as
`<tx_hash>#<index>`; outputs with bech32 addresses (base58 for Byron),
lovelace and native assets; the fee; minted (positive) and burned (negative)
assets; the validity interval; and the required signers. Anything else is an
`internal` error.

Nothing the engine logs contains an argument value or an API key.

## Shared contracts

- `fluent_core::address::inspect`: decodes one address into an
  `AddressReport` (the JSON above, deriving `schemars::JsonSchema`). Malformed
  input is `invalid_arguments` naming the `address` argument; neither the
  message nor the details quote the input.
- `fluent_core::config::Config`: the model above; `Config::load`,
  `Config::redacted`.
- `fluent_core::error::FluentError`: `code()` returns a stable `ErrorCode`
  (`invalid_arguments`, `unknown_transaction`, `unknown_protocol`,
  `registration_unavailable`, `network_mismatch`, `insufficient_funds`,
  `input_not_resolved`, `script_failure`, `resolver_timeout`,
  `resolver_unavailable`, `quota_exhausted`, `unauthorized`, `internal`),
  `message()` is for people, and `details()` is optional JSON. None of them
  contains credentials or complete argument values. `Violation`
  (`path`, `message`) and `InputDiagnostic` (counts only) carry the
  structured details of `invalid_arguments` and unresolved inputs.
- `fluent_core::envelope::PreparedTransaction`: the unsigned-transaction
  result. `status`, `signed`, `submitted` and `next_steps` are fixed values, and
  it derives `schemars::JsonSchema` for use as an MCP output schema.
- `fluent_core::registration`: the async `load_dir` returns a `Catalog` (slug
  → `Arc<Registration>`, with `get` and `iter`) and the rejected bundles;
  `load_dir_with` takes an `OciFetcher`, for example one with another timeout.
  `Registration` exposes its `Manifest`, `SkillDocument`, TII, digests,
  revision and, for registry bundles, `provenance()`, and cannot be changed
  once loaded.
- `fluent_core::registry`: `OciFetcher::fetch_tii(&RegistrySource)` returns a
  `FetchedArtifact` (the verified TII bytes, its `Provenance` and whether it
  came from the cache) or a `FetchError` naming the failed check.
- `fluent_core::catalog`: `build_tools(&Registration)` and `fixed_tools()`
  return `ToolDescriptor`s (`name`, `title`, `description`, `input_schema`,
  `output_schema`, `annotations`, `registration_slug`, `tx_name`);
  `all_tools(&Catalog)` returns both and rejects duplicate names.
  `catalog::schema::inline` is the pure `$ref` inliner, and `SkillResult` is
  the `fluent_get_skill` result.
- `fluent_core::engine`: `Engine::new(&Config, &Catalog)`,
  `Engine::prepare(PrepareRequest)`, `ArgumentRules` (the argument checks of
  one transaction) and `resolver::SCRIPT_LOG_LINES`.
- `fluent_core::summary`: `decode(tx_hex) -> Summary { tx_hash, transaction }`.
- `fluent_server::mcp`: `FluentHandler` (the rmcp `ServerHandler` over an
  `Arc<Catalog>`, an `Arc<Engine>` and a `ToolScope`), `ToolScope`
  (`visible_slugs`, the registrations one session sees), `AllRegistrations`
  (every loaded one), `INSTRUCTIONS` (at most 512 characters) and
  `error_json`. `FluentHandler::for_session` gives each HTTP session its own
  handler, whose `principal()` is the `Principal` that initialized it;
  `request_principal` reads the one attached to a request.
- `fluent_server::http`: `HttpServer` (`bind`, `local_addr`, `run`),
  `router`, `shutdown_signal` and `MAX_BODY_BYTES`; `http::auth`:
  `Authenticator`, the `require_auth` middleware, `Principal` (`sub`,
  `email`) and `ProtectedResourceMetadata`.

## License

Apache-2.0. See [LICENSE](LICENSE).
