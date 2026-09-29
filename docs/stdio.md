# Tx3 Fluent over stdio

`fluent serve --stdio` runs Fluent as a child process of one MCP client: the
client writes MCP messages to its stdin and reads them from its stdout. There
is no port, no authentication and no quota; the user who starts the process
uses it. The configuration and bundles are the same as for HTTP; see
[the self-hosting guide](self-hosting.md) for writing them. Over stdio,
`[server]`, `[auth]`, `[store]` and `[site]` take no effect: the server
listens nowhere, checks no credentials, keeps no daily quota and serves no
site. `[limits]` still bound each call's duration and concurrency.

Install the binary from a checkout of this repository:

```sh
cargo install --locked --path crates/fluent-server
```

Check the configuration and bundles first. A desktop client shows little of
a server that fails to start, and every bundle rejected here is missing from
its tool list:

```sh
fluent registrations check --config /path/to/fluent.toml
```

Paths in the configuration resolve against the working directory, which a
desktop client does not choose for you: write `[registrations].dir` as an
absolute path. Logs go to stderr as JSON lines; stdout carries only MCP
messages. Registrations load when the process starts, so after changing a
bundle restart the server from the client (or restart the client).

## Claude Desktop

Add the server to `claude_desktop_config.json` (Settings → Developer → Edit
Config; on macOS `~/Library/Application Support/Claude/claude_desktop_config.json`,
on Windows `%APPDATA%\Claude\claude_desktop_config.json`), then restart
Claude Desktop:

```json
{
  "mcpServers": {
    "tx3-fluent": {
      "command": "/Users/you/.cargo/bin/fluent",
      "args": ["serve", "--stdio", "--config", "/Users/you/fluent/fluent.toml"],
      "env": {
        "TRP_PREPROD_API_KEY": "…"
      }
    }
  }
}
```

Claude Desktop does not read your shell profile, so `command` is the absolute
path (`which fluent` prints it) and every variable the configuration names in
a `*_env` key goes in `env`. Server logs are in Claude Desktop's MCP log
directory (`~/Library/Logs/Claude/mcp-server-tx3-fluent.log` on macOS).

To run the container image instead of an installed binary, have the client
start it with `-i` (keep stdin open) and without `-t`:

```json
{
  "mcpServers": {
    "tx3-fluent": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "TRP_PREPROD_API_KEY",
        "-v", "/Users/you/fluent:/data",
        "ghcr.io/tx3-lang/tx3-fluent:latest",
        "fluent", "serve", "--stdio", "--config", "/data/fluent.toml"
      ],
      "env": {
        "TRP_PREPROD_API_KEY": "…"
      }
    }
  }
}
```

Here the configuration's paths are the container's, as in the self-hosting
quick start.

## MCP Inspector

[MCP Inspector](https://github.com/modelcontextprotocol/inspector) 2.x starts
the server itself. Put the server command after `--`, so Inspector does not
take `--config` as its own option:

```sh
npx @modelcontextprotocol/inspector -- fluent serve --stdio --config fluent.toml
```

It opens a browser UI; connect, then list and call the tools. Its `--cli`
mode takes the server command first, then `--` and Inspector's options:

```sh
npx @modelcontextprotocol/inspector --cli fluent serve --stdio --config fluent.toml \
  -- --method tools/list
npx @modelcontextprotocol/inspector --cli fluent serve --stdio --config fluent.toml \
  -- --method tools/call --tool-name fluent_get_skill --tool-arg protocol=transfer_preprod
```

## Other clients

Any client that launches stdio servers takes the same command and arguments.
For example, Claude Code:

```sh
claude mcp add tx3-fluent -e TRP_PREPROD_API_KEY=… -- \
  fluent serve --stdio --config /path/to/fluent.toml
```
