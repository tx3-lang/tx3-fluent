# Journey evidence

How to produce the acceptance evidence for the Tx3 Fluent spike: real ChatGPT
sessions against the hosted deployment, a transcript of every tool the
sessions listed and called, rendered from the deployment's logs, and an
independent decode check of each prepared transaction. Only ChatGPT sessions count as acceptance evidence. An
MCP Inspector run (see [Dry run](#dry-run-with-mcp-inspector)) checks these
steps but proves nothing about the journeys.

The evidence covers six items:

| # | Item | What it shows |
| --- | --- | --- |
| E1 | [Two-account isolation](#e1-two-account-isolation) | An account sees and calls only the registrations it selected. |
| E2 | [Metadata refresh](#e2-metadata-refresh) | A changed selection reaches ChatGPT after a connector refresh. |
| E3 | [Transfer journey](#e3-transfer-journey) | ChatGPT gets the skill, gathers inputs and prepares an unsigned transfer. |
| E4 | [Strike journey](#e4-strike-journey) | Out of the spike's scope: no hosted Strike bundle. |
| E5 | [Decode check](#e5-decode-check) | Each prepared transaction pays what was asked, on the asked network. |
| E6 | [Unsupported requests](#e6-unsupported-requests) | Requests Fluent cannot serve are declined or refused, and nothing is signed or submitted. |

## Before you start

- **Deployment.** The hosted deployment serves
  [`examples/config/hosted.toml`](../examples/config/hosted.toml) at
  `https://fluent.tx3.land`, with the bundles in
  [`deploy/hosted/registrations`](../deploy/hosted/registrations). See
  [the hosted deployment guide](hosted-deployment.md).
- **Two accounts.** Accounts A and B sign in to the identity provider with
  different subjects. ChatGPT needs developer mode on. One ChatGPT account
  can serve both, one at a time: to switch, disconnect the Fluent connector
  in ChatGPT's settings, sign out of the site and of the identity provider
  (for Auth0, `https://<tenant>/v2/logout`), then reconnect and sign in as
  the other account. Without the identity-provider sign-out, the connector
  signs straight back in as the previous account. Disconnect rather than
  delete: a new connector identifies itself with a new client metadata
  document URL, which an identity provider that registers such clients by
  hand (Auth0 does) must import again.
- **Addresses.** A funded preprod address for A (the sender) and a second
  preprod address (the receiver).
- **Evidence directory.** One directory per session date, for example
  `evidence/2026-10-01/`, outside this repository. It holds the saved logs,
  the transcript, both conversation exports and the decode checks.
- **A checkout of this repository** with the Rust toolchain, for
  `cargo xtask verify` and `cargo xtask transcript`. These are development
  tools: they are not part of the `fluent` binary or the container image.

## Record the session

Nothing about the deployment changes for recording: no special serve mode,
no rollout. The server already logs, at `info`, one line per `tools/list`
(the account's hashed subject and the tool names listed) and one per tool
call (the tool, the argument **names**, the outcome and the duration). The
logs never hold argument values, results or transaction hashes; see
[What is logged](operations.md#what-is-logged).

After the session, save the deployment's logs for its time window and render
the transcript offline:

```sh
kubectl -n <namespace> logs deploy/<release> --since-time=2026-10-01T14:00:00Z \
  > evidence/2026-10-01/fluent.log
cargo xtask transcript --logs evidence/2026-10-01/fluent.log \
  --out evidence/2026-10-01/transcript.md
```

Each `tools/list` and `tools/call` becomes one numbered section with its
time, the principal (`sub:<hash>`), the tools listed or the tool called, its
argument names and outcome. Other log lines are skipped. `--logs` can be
repeated, for several pods or restarts, and entries are ordered by time.
`--sub-hash <hash>` (repeatable) keeps only those accounts. Without
`--logs`, the logs are read from stdin.

The hashed subject of an account is the first 12 hex digits of the SHA-256 of
its `sub`:

```sh
printf %s 'auth0|…' | shasum -a 256 | cut -c1-12
```

Note each account's hash in the evidence notes, so the transcript's
principals can be attributed. Pair each section with the conversation export
by time, principal and tool.

## E1. Two-account isolation

1. As A, sign in at `https://fluent.tx3.land`, enable `transfer_preprod` on
   **Protocols**, and connect ChatGPT as **Connect** describes.
2. As B, sign in, enable nothing, and connect ChatGPT the same way (with a
   single ChatGPT account, switch as [Before you start](#before-you-start)
   describes).
3. In B's ChatGPT, ask: *"Send 5 ADA on preprod from addr_test1… to
   addr_test1…"*.

Expected: B's connector lists only `fluent_get_skill` and
`fluent_inspect_address`, and ChatGPT says it cannot prepare a transfer. If
ChatGPT calls the transfer tool anyway, the call fails as
`registration_unavailable`. In the transcript, B's `tools/list` has 2 tools and
A's has 3, and any call B makes to the transfer tool has outcome
`registration_unavailable`.

## E2. Metadata refresh

Run it as either account; the steps below use B.

1. As B, enable `transfer_preprod` on **Protocols**.
2. In ChatGPT's settings, refresh B's connector.
3. Ask B's ChatGPT which Fluent tools it has.
4. Disable `transfer_preprod` again and refresh.

Expected: after step 2, B's connector lists `transfer_preprod_transfer`.
After step 4, it no longer does. Each refresh is a `tools/list` section in the
transcript, with the tool count changing.

## E3. Transfer journey

In A's ChatGPT, run each request below in a fresh conversation. Supply
addresses only when ChatGPT asks for them.

| Kind | Request |
| --- | --- |
| Direct | *"Send 5 ADA to addr_test1… on preprod."* |
| Paraphrase | *"I want to pay my friend five ada on the test network."* |
| Follow-up | After a prepared transfer: *"Actually make it 7 ADA."* |
| Ambiguous | *"Send some ADA to my friend."* |

Expected, for each:

- ChatGPT calls `fluent_get_skill` for `transfer_preprod` before the transfer
  tool. It may call `fluent_inspect_address` to check an address's network.
- It asks for anything missing (the sender, the receiver, the amount) instead
  of guessing, and converts ADA to lovelace (5 ADA is `5000000`).
- `transfer_preprod_transfer` returns `prepared_unsigned`, and ChatGPT
  presents the summary as a transaction to review and sign in a wallet,
  never as a completed payment.

Export each conversation, and run [E5](#e5-decode-check) on every prepared
transaction.

## E4. Strike journey

Out of the spike's scope: the founder narrowed it to the transfer journey on
2026-09-30. The Strike journey needs a hosted Strike bundle, and there is
none: `open-tx3/strike-staking:0.2.0` is not published, so
[`deploy/hosted/registrations`](../deploy/hosted/registrations) holds only
`transfer_preprod`. Do not substitute another source or a mocked resolution.

When a Strike bundle is deployed, repeat [E3](#e3-transfer-journey) with Strike: A enables
the Strike registration and asks ChatGPT to stake, add to a stake or withdraw
a stake. ChatGPT must read the skill first, obtain the STRIKE balance and
staking UTxO it declares as external dependencies from the user (or say it
cannot), derive the owner's key hash with `fluent_inspect_address`, and
return an unsigned transaction.

## E5. Decode check

For each prepared transaction, copy `unsigned_tx_cbor_hex` from the tool
result in ChatGPT, then check it independently of Fluent's summary:

```sh
cargo xtask verify --cbor <hex> \
  --expect-output '<receiver>=<lovelace>' \
  --expect-network preprod \
  > evidence/2026-10-01/verify-<tx_hash>.json
```

For native assets, add `+<policy_id>.<asset_name_hex>=<amount>` to the
output. For a transaction that needs a signature, add
`--expect-signer <key hash>`. `--cbor -` reads the CBOR from stdin.

`cargo xtask verify` decodes the CBOR on its own, with pallas directly: not
with the arguments, the resolver or the `fluent` summary decoder that built
the envelope. It prints the decoded summary, one check per
expectation and the verdict, and exits 0 only on `match`:

- each expected output is matched by its own output, with exactly the
  expected address, lovelace and assets;
- every output address is on the expected network (addresses tell mainnet
  from a testnet, not preprod from preview);
- each expected signer is a required signer.

This establishes that the transaction pays what the user asked, to whom they
asked, on the network they asked. It does not establish that the inputs are
still unspent, that the fee is acceptable, what the change and other outputs
do, or that the transaction will validate. Those remain for the wallet
review before signing.

## E6. Unsupported requests

In A's ChatGPT, ask:

- *"Sign and submit that transaction for me."*
- *"Send 5 ADA on mainnet."* (no mainnet transfer is registered)
- *"Swap 100 ADA for STRIKE."* (no swap is registered)

Expected: ChatGPT says Fluent prepares unsigned transactions only and cannot
do these, without claiming anything happened on chain. If it calls a tool
Fluent does not have, the logs record the call, and the transcript shows it
with outcome `unknown_tool`.

## Evidence bundle

Fill in one row per item. Transcript sections are cited by their number; keep
the saved logs beside the transcript they were rendered from.

| # | Conversation export | Transcript sections | Decode checks | Result and notes |
| --- | --- | --- | --- | --- |
| E1 | *founder: A and B exports* | *founder* | n/a | *founder* |
| E2 | *founder* | *founder* | n/a | *founder* |
| E3 | *founder: one per request* | *founder* | *founder: one per transaction* | *founder* |
| E4 | n/a: out of scope (no hosted Strike bundle) | n/a | n/a | n/a |
| E5 | n/a | n/a | *founder* | *founder* |
| E6 | *founder* | *founder* | n/a | *founder* |

Account hashes: A `sub:…`, B `sub:…` (*founder*).

## Dry run with MCP Inspector

These steps can be rehearsed against a local hosted-mode instance with
[MCP Inspector](https://github.com/modelcontextprotocol/inspector). This is
not acceptance evidence: Inspector sends the calls it is told to, so no model
chooses them.

1. Serve the hosted bundles in `oidc` mode with a `[store]` with
   `fluent serve --http`, saving stderr to a log file, with a copy of
   `hosted.toml` that listens on
   `127.0.0.1:8080` with `public_url = "http://127.0.0.1:8080"` and the site
   disabled. Point `[auth]` at any issuer whose tokens you can mint, for
   example a local JWKS, and `audience` at `http://127.0.0.1:8080/mcp`.
2. Select `transfer_preprod` for account A. Without the site, insert the
   selection into the store with its current revision, which
   `fluent registrations check` prints:

   ```sh
   sqlite3 fluent.sqlite \
     "INSERT INTO users (sub, created_at) VALUES ('<A sub>', strftime('%s'));
      INSERT INTO selections (sub, slug, revision, selected_at)
      VALUES ('<A sub>', 'transfer_preprod', '<revision>', strftime('%s'));"
   ```

3. Drive each item with Inspector's CLI, as A or B:

   ```sh
   npx @modelcontextprotocol/inspector --cli http://127.0.0.1:8080/mcp \
     --transport http --header "Authorization: Bearer $TOKEN_A" \
     --method tools/call --tool-name transfer_preprod_transfer \
     --tool-arg quantity=3000000 --tool-arg sender=addr_test1… --tool-arg receiver=addr_test1…
   ```

   Inspector refuses to call a tool it did not list, so send B's transfer and
   the unsupported calls as plain MCP requests (for example with `curl`) to
   see the server's refusals in the transcript.
4. Stop the server and render the transcript from the saved log file with
   `cargo xtask transcript`, as in [Record the session](#record-the-session).
5. Run [E5](#e5-decode-check) on the prepared CBOR.
