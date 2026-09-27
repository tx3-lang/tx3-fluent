# Tx3 Fluent

Tx3 Fluent turns published Tx3 protocols into tools that agents and people can
call to prepare Cardano transactions. It resolves each request through a TRP
endpoint and returns an **unsigned, unsubmitted** transaction with a readable
summary. Fluent never holds keys, signs or submits: a wallet does that after the
user has reviewed the transaction.

This repository is at its foundation stage. It holds the workspace layout and
the contracts shared by every later component: the configuration model, the
error type and the result envelope, plus offline address inspection.
Registration loading, tool generation, resolution, transports, authentication,
storage and the site are added later.

## Layout

| Path | Contents |
| --- | --- |
| `crates/fluent-core` | Library: configuration, errors, result envelopes and address inspection; later registration, catalog and engine. |
| `crates/fluent-server` | The `fluent` binary: CLI; later MCP transports, HTTP, store and site. |
| `examples/config` | Example configurations, loaded by the tests. |

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
| `dir` | path | required | Directory holding protocol registrations. |

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
  contains credentials or complete argument values.
- `fluent_core::envelope::PreparedTransaction`: the unsigned-transaction
  result. `status`, `signed`, `submitted` and `next_steps` are fixed values, and
  it derives `schemars::JsonSchema` for use as an MCP output schema.

## License

Apache-2.0. See [LICENSE](LICENSE).
