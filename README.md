# Tx3 Fluent

Tx3 Fluent turns published Tx3 protocols into tools that agents and people can
call to prepare Cardano transactions. It resolves each request through a TRP
endpoint and returns an **unsigned, unsubmitted** transaction with a readable
summary. Fluent never holds keys, signs or submits: a wallet does that after the
user has reviewed the transaction.

This repository is at an early stage. It holds the workspace layout, the
contracts shared by every later component (the configuration model, the error
type and the result envelope), the registration bundle loader with its
registry artifact fetch, and the tool catalog. Resolution, transports,
authentication, storage and the site are added later.

## Layout

| Path | Contents |
| --- | --- |
| `crates/fluent-core` | Library: configuration, errors, result envelopes, registration bundles, tool catalog; later engine and address utilities. |
| `crates/fluent-server` | The `fluent` binary: CLI; later MCP transports, HTTP, store and site. |
| `examples/config` | Example configurations, loaded by the tests. |
| `crates/fluent-core/tests/fixtures/registrations` | Valid and invalid registration bundles, loaded by the tests. |
| `crates/fluent-core/tests/fixtures/tii` | TII files used without a registration, such as the SDK spec's `complex.tii`. |
| `crates/fluent-core/tests/golden` | Reviewed tool descriptor lists the catalog tests compare against. |

## Build and test

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.91 with `rustfmt` and
`clippy`); `rustup` installs it on first use.

```sh
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

CI runs the last three on every pull request and push to `main`; each job
blocks.

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

## Configuration reference

Fluent reads one TOML file, passed with `--config`. **Unknown keys are errors.**
Keys ending in `_env` hold the *name* of an environment variable, never a
secret. Fluent reads the variable's value at startup, keeps it in memory, and
never serializes it. An empty value counts as unset. Redacted output shows
`"<set>"` or `"<unset>"` in place of these keys.

### `[server]` (optional)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `listen` | socket address | `"127.0.0.1:8080"` | Address and port to listen on. |
| `public_url` | URL | none | Externally visible base URL. |

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
| `global_cutoff_secs` | integer | `45` | Seconds one request may take end to end. |
| `max_concurrent_resolutions` | integer | `8` | Resolver calls in flight at once, server-wide. |
| `per_user_daily_quota` | integer | `200` | Prepared transactions one caller may request per day. |

### `[auth]` (required)

`mode` selects how callers authenticate, and only that mode's keys are allowed.

| `mode` | Keys | Meaning |
| --- | --- | --- |
| `"none"` | none | Every caller is accepted. |
| `"token"` | `token_env` (variable name, required) | Callers present one static bearer token. |
| `"oidc"` | `issuer` (URL), `jwks_url` (URL), `audience` (string), all required | Callers present a JWT from an OpenID Connect issuer. |

### `[store]` (optional)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `sqlite_path` | path | required | SQLite database file. |

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
  deployment with OIDC, mainnet and preprod, and the site enabled.

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

## Shared contracts

- `fluent_core::config::Config`: the model above; `Config::load`,
  `Config::redacted`.
- `fluent_core::error::FluentError`: `code()` returns a stable `ErrorCode`
  (`invalid_arguments`, `unknown_transaction`, `unknown_protocol`,
  `registration_unavailable`, `network_mismatch`, `insufficient_funds`,
  `input_not_resolved`, `script_failure`, `resolver_timeout`,
  `resolver_unavailable`, `quota_exhausted`, `unauthorized`, `internal`),
  `message()` is for people, and `details()` is optional JSON. None of them
  contains credentials or complete argument values.
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

## License

Apache-2.0. See [LICENSE](LICENSE).
