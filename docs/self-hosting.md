# Self-hosting Tx3 Fluent

This guide is for an operator who runs their own Fluent: the container image
over HTTP, or the `fluent` binary as a local stdio process for a desktop MCP
client. Both are the same engine and read the same configuration and
registration bundles. The [README](../README.md) is the reference for every
configuration key and bundle rule; this guide walks through them in order.

## Prerequisites

- For HTTP in a container: Docker (or any OCI runtime). The image is
  `ghcr.io/tx3-lang/tx3-fluent`, for `linux/amd64` and `linux/arm64`: `latest`
  follows `main`, `vX.Y.Z` and `vX.Y` follow releases, and `sha-<commit>` names
  one build.
- For stdio, or to run the `fluent` CLI on the host: Rust (`rustup`) and a
  checkout of this repository. Build and install the binary with

  ```sh
  cargo install --locked --path crates/fluent-server
  ```

  which puts `fluent` in `~/.cargo/bin`. The pinned toolchain in
  `rust-toolchain.toml` is installed on first use.
- A TRP endpoint for each network you prepare transactions on, such as
  `https://cardano-preprod.trp-m1.demeter.run`, and its API key when it needs
  one.
- One registration bundle per protocol deployment you want to offer.

## Quick start

From a checkout of this repository, serve the fixture transfer registration
on preprod.

1. Make a directory for the server and copy the bundle into it:

   ```sh
   mkdir -p ~/fluent/registrations
   cp -R crates/fluent-core/tests/fixtures/registrations/valid/transfer_preprod ~/fluent/registrations/
   ```

2. Write `~/fluent/fluent.toml`:

   ```toml
   [server]
   listen = "0.0.0.0:8080"

   [registrations]
   dir = "/data/registrations"

   [networks.preprod]
   trp_url = "https://cardano-preprod.trp-m1.demeter.run"
   trp_api_key_env = "TRP_PREPROD_API_KEY"

   [auth]
   mode = "token"
   token_env = "FLUENT_API_TOKEN"
   ```

   The paths are the container's: `~/fluent` is mounted at `/data`.

3. Start the container, with a fresh bearer token. To try the source before a
   release, build the image yourself with `docker build -t tx3-fluent .` and
   use `tx3-fluent` in place of the image name.

   ```sh
   export FLUENT_API_TOKEN=$(openssl rand -hex 32)
   docker run -d --name fluent -p 127.0.0.1:8080:8080 \
     -e FLUENT_API_TOKEN -e TRP_PREPROD_API_KEY \
     -v ~/fluent:/data ghcr.io/tx3-lang/tx3-fluent:latest
   ```

4. Check it:

   ```sh
   curl http://127.0.0.1:8080/healthz
   npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8080/mcp \
     --transport http --header "Authorization: Bearer $FLUENT_API_TOKEN" --method tools/list
   ```

   `healthz` answers `{"status":"ok","version":"…"}` and the tool list holds
   `transfer_preprod_transfer`, `fluent_get_skill` and
   `fluent_inspect_address`. `docker logs fluent` shows the startup: one line
   per rejected bundle, then `serving MCP over HTTP` with the number of
   registrations and tools.

5. For stdio, install `fluent` as above and write a copy of the
   configuration that names the bundles by their host path:

   ```sh
   sed "s#/data/registrations#$HOME/fluent/registrations#" ~/fluent/fluent.toml > ~/fluent/stdio.toml
   fluent registrations check --config ~/fluent/stdio.toml
   npx @modelcontextprotocol/inspector --cli fluent serve --stdio --config ~/fluent/stdio.toml \
     -- --method tools/list
   ```

   Then register it with a desktop client as [the stdio guide](stdio.md)
   shows.

The same setup with Docker Compose is [`docker-compose.yml`](../docker-compose.yml)
at the repository root: put `fluent.toml` and `registrations/` next to it and
run `docker compose up -d`.

## The container

The image runs `fluent serve --http --config /data/fluent.toml` as user `fluent`
(uid 1000) and exposes port 8080.

| Path | Holds |
| --- | --- |
| `/data/fluent.toml` | The configuration. Required. |
| `/data/registrations` | The bundles, when `[registrations].dir` points here. |
| `/data/fluent.sqlite` | The store, when `[store].sqlite_path` points here. |

`/data` is a volume. Mount a host directory or a named volume over it, or
mount the configuration and the bundles separately as the Compose file does.
The server writes to `/data` only for the store and for the registry cache
(`registrations/.cache`), so both must be writable by uid 1000; on Linux,
`chown -R 1000:1000` the host directory or run the container with
`--user "$(id -u):$(id -g)"`. A read-only bundle directory works; registry
artifacts are then fetched on every start.

The server must listen on every interface inside the container: set
`[server].listen = "0.0.0.0:8080"`, or `FLUENT_SERVER__LISTEN=0.0.0.0:8080`.
With the default `127.0.0.1:8080` it starts but the published port reaches
nothing. `auth.mode = "none"` refuses a non-loopback address, so a container
uses `token` or `oidc`.

Any configuration key can instead come from a `FLUENT_*` variable (see
[Environment overrides](../README.md#environment-overrides)), and every
secret comes from the variable its `*_env` key names. Pass them with `-e`,
never in `fluent.toml`.

Other commands run in the same image, for example:

```sh
docker run --rm -v ~/fluent:/data ghcr.io/tx3-lang/tx3-fluent:latest \
  fluent registrations check --config /data/fluent.toml
```

## Authoring a bundle

A bundle is a directory under `[registrations].dir` with three files:

```text
registrations/
  my_protocol_preprod/
    registration.toml
    SKILL.md
    protocol.tii
```

1. **The TII.** Copy the protocol's TII file into the bundle as
   `protocol.tii`. Its `protocol` block names the scope, name and version the
   manifest must repeat, and it must define the deployment profile you use.
   To take the TII from the registry instead, leave it out and set
   `source = "registry"`; see
   [Registry artifacts](../README.md#registry-artifacts).

2. **`registration.toml`**, the manifest:

   ```toml
   slug = "my_protocol_preprod"   # unique; ^[a-z][a-z0-9_]{2,40}$; prefixes the tool names

   [protocol]                      # exactly the TII's protocol block
   scope = "my-org"
   name = "my-protocol"
   version = "0.1.0"
   source = "local"

   [artifact]
   tii = "protocol.tii"            # the default
   tii_digest = "sha256:…"         # optional for local bundles; checked when set

   [skill]
   path = "SKILL.md"               # the default

   [deployment]
   profile = "preprod"             # mainnet, preprod or preview; defined in the TII
   network = "preprod"             # the profile's network, and a [networks.<name>] table
   ```

3. **`SKILL.md`**, the consumption skill an assistant reads through
   `fluent_get_skill` before calling the protocol's tools. YAML frontmatter,
   then Markdown:

   ```markdown
   ---
   name: my-protocol
   description: What the protocol does, ending with when to use this skill.
   protocol: my-org/my-protocol:0.1.0
   tii_digest: sha256:…
   network: preprod
   revision: 1
   dependencies:
     - id: user_address
       description: The user's bech32 preprod address.
       required_for: [transfer]
   ---
   # my-protocol

   How to call each transaction, what to ask the user, what to check in the
   summary before they sign.
   ```

   `protocol`, `tii_digest` and `network` must match the bundle.
   `dependencies` lists what the assistant must obtain elsewhere, and each
   `required_for` names transactions the TII defines. Bump `revision` when
   you change the skill.

4. **Digests.** `tii_digest` is `sha256:` followed by the SHA-256 of the TII
   file's exact bytes:

   ```sh
   echo "sha256:$(sha256sum protocol.tii | cut -d' ' -f1)"   # shasum -a 256 on macOS
   ```

   Then check every bundle with the configuration you serve:

   ```sh
   fluent registrations check --config fluent.toml
   ```

   It prints a `[[registration]]` table per accepted bundle with its
   `tii_digest`, `skill_digest` and `revision`, and a `[[rejected]]` table per
   rejected bundle with the broken rule. A mismatched digest is reported with
   both values, for example `incompatible skill binding: skill tii_digest is
   sha256:…, the TII is sha256:…`. The command exits non-zero while any
   bundle is rejected. The revision changes whenever the TII, the skill, the
   profile or the network does; hosted users must re-enable a registration
   whose revision changed.

The full rules are in [Registration bundles](../README.md#registration-bundles).

## Networks and TRP

Each `[networks.<name>]` table gives the TRP endpoint that resolves
transactions on that network, and optionally the variable holding its API
key. A registration's `deployment.network` names one of these tables; a
registration whose network has no table loads, but its transactions fail
until one is configured.

```toml
[networks.mainnet]
trp_url = "https://cardano-mainnet.trp-m1.demeter.run"
trp_api_key_env = "TRP_MAINNET_API_KEY"

[networks.devnet]
trp_url = "http://host.docker.internal:8164"   # a devnet on the Docker host
```

`trp_url` must not embed credentials. Tune the resolver timeout and the other
limits in `[limits]`; see [the operations guide](operations.md).

## Authentication

`[auth].mode` sets who may call `/mcp`:

| Mode | Use | Configure |
| --- | --- | --- |
| `none` | One user on the same machine. Only loopback listen addresses; never in a container. | `mode = "none"` |
| `token` | A small set of trusted clients. | `mode = "token"`, `token_env` naming the variable with the token. Clients send `Authorization: Bearer <token>`. |
| `oidc` | A public server. Callers sign in with an OpenID Connect issuer. | `issuer`, `jwks_url`, `audience`, and `[server].public_url`. See [the hosted deployment](hosted-deployment.md). |

Generate a token with `openssl rand -hex 32`. A stdio server has no
authentication: whoever starts the process uses it.

## Restart to reload

Fluent reads the configuration and loads every bundle once, at startup. To
add, change or remove a bundle, or change the configuration, edit the files
and restart:

```sh
docker restart fluent            # or: docker compose restart fluent
```

A stdio client restarts the server when you restart the client or reconnect
the server. Sessions live in memory, so a restart ends them; clients
reconnect and list the tools again.

## Upgrading

1. Read the release notes for configuration or bundle changes.
2. Pull the new image, or pin its version:
   `docker pull ghcr.io/tx3-lang/tx3-fluent:vX.Y.Z`.
3. Check the configuration and bundles with the new binary before switching:

   ```sh
   docker run --rm -v ~/fluent:/data ghcr.io/tx3-lang/tx3-fluent:vX.Y.Z \
     fluent config check --config /data/fluent.toml
   docker run --rm -v ~/fluent:/data ghcr.io/tx3-lang/tx3-fluent:vX.Y.Z \
     fluent registrations check --config /data/fluent.toml
   ```

4. Replace the container (`docker compose pull && docker compose up -d`).
   The store migrates itself at startup; back up the SQLite file first.

For a stdio install, pull the repository and run
`cargo install --locked --path crates/fluent-server` again.
