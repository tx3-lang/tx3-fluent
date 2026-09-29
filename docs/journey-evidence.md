# Journey evidence

How to produce the acceptance evidence for the Tx3 Fluent spike: real ChatGPT
sessions against the hosted deployment, a server-side transcript of every tool
the sessions listed and called, and an independent decode check of each
prepared transaction. Only ChatGPT sessions count as acceptance evidence. An
MCP Inspector run (see [Dry run](#dry-run-with-mcp-inspector)) checks these
steps but proves nothing about the journeys.

The evidence covers six items:

| # | Item | What it shows |
| --- | --- | --- |
| E1 | [Two-account isolation](#e1-two-account-isolation) | An account sees and calls only the registrations it selected. |
| E2 | [Metadata refresh](#e2-metadata-refresh) | A changed selection reaches ChatGPT after a connector refresh. |
| E3 | [Transfer journey](#e3-transfer-journey) | ChatGPT gets the skill, gathers inputs and prepares an unsigned transfer. |
| E4 | [Strike journey](#e4-strike-journey) | The same for Strike staking. |
| E5 | [Decode check](#e5-decode-check) | Each prepared transaction pays what was asked, on the asked network. |
| E6 | [Unsupported requests](#e6-unsupported-requests) | Requests Fluent cannot serve are declined or refused, and nothing is signed or submitted. |

## Before you start

- **Deployment.** The hosted deployment serves
  [`examples/config/hosted.toml`](../examples/config/hosted.toml) at
  `https://fluent.tx3.land`, with the bundles in
  [`deploy/hosted/registrations`](../deploy/hosted/registrations). See
  [the hosted deployment guide](hosted-deployment.md).
- **Two accounts.** Accounts A and B sign in to the identity provider with
  different subjects. Each has its own ChatGPT account with developer mode on.
- **Addresses.** A funded preprod address for A (the sender) and a second
  preprod address (the receiver). For Strike, a mainnet address that holds
  STRIKE, or an existing staking position.
- **Evidence directory.** One directory per session date, for example
  `evidence/2026-10-01/`, outside this repository. It holds the transcript,
  both conversation exports and the decode checks.
- **The `fluent` binary**, from a release or `cargo build --release`, for
  `fluent verify`.

## Record the session

`fluent demo record` serves exactly as `fluent serve` does and also writes a
Markdown transcript: one section per `tools/list` and `tools/call` answered,
with the account's hashed subject (`sub:…`, the same `sub_hash` the logs use),
tool and argument names, the outcome and the redacted result. Argument values
are never recorded. Addresses are shortened to their ends. The transaction
CBOR, address hex and skill bodies are replaced by their length. Transaction
hashes, amounts and error codes are kept, so each section can be matched to
the conversation.

For the session, run the deployment's container with the recording command
instead of its default (a rollout, so a founder act), then restore the
default afterwards:

```sh
fluent demo record --config /data/fluent.toml --out /data/evidence
```

The server prints `recording the transcript to /data/evidence/transcript-<UTC time>.md`
on stderr. Copy that file into the evidence directory when the session ends,
for example with `kubectl cp`. Every restart starts a new transcript. The
hashed subject of an account is the first 12 hex digits of the SHA-256 of its
`sub`:

```sh
printf %s 'auth0|…' | shasum -a 256 | cut -c1-12
```

Note each account's hash in the evidence notes, so the transcript's
principals can be attributed.

## E1. Two-account isolation

1. As A, sign in at `https://fluent.tx3.land`, enable `transfer_preprod` on
   **Protocols**, and connect ChatGPT as **Connect** describes.
2. As B, sign in, enable nothing, and connect ChatGPT the same way.
3. In B's ChatGPT, ask: *"Send 5 ADA on preprod from addr_test1… to
   addr_test1…"*.

Expected: B's connector lists only `fluent_get_skill` and
`fluent_inspect_address`, and ChatGPT says it cannot prepare a transfer. If
ChatGPT calls the transfer tool anyway, the call fails as
`registration_unavailable`. In the transcript, B's `tools/list` has 2 tools and
A's has 3.

## E2. Metadata refresh

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

The Strike journey needs a hosted Strike bundle. There is none yet:
`open-tx3/strike-staking:0.2.0` is not published, so
[`deploy/hosted/registrations`](../deploy/hosted/registrations) holds only
`transfer_preprod`. Until the bundle exists, record its absence as a spike
finding. Do not substitute another source or a mocked resolution.

Once it is deployed, repeat [E3](#e3-transfer-journey) with Strike: A enables
the Strike registration and asks ChatGPT to stake, add to a stake or withdraw
a stake. ChatGPT must read the skill first, obtain the STRIKE balance and
staking UTxO it declares as external dependencies from the user (or say it
cannot), derive the owner's key hash with `fluent_inspect_address`, and
return an unsigned transaction.

## E5. Decode check

For each prepared transaction, copy `unsigned_tx_cbor_hex` from the tool
result in ChatGPT, then check it independently of Fluent's summary:

```sh
fluent verify --cbor <hex> \
  --expect-output '<receiver>=<lovelace>' \
  --expect-network preprod \
  > evidence/2026-10-01/verify-<tx_hash>.json
```

For native assets, add `+<policy_id>.<asset_name_hex>=<amount>` to the
output. For a transaction that needs a signature, add
`--expect-signer <key hash>`. `--cbor -` reads the CBOR from stdin.

`fluent verify` decodes the CBOR on its own, without the arguments or the
resolver that produced it. It prints the decoded summary, one check per
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
Fluent does not have, the transcript records the call with outcome
`unknown_tool`.

## Evidence bundle

Fill in one row per item. Transcript sections are cited by their number.

| # | Conversation export | Transcript sections | Decode checks | Result and notes |
| --- | --- | --- | --- | --- |
| E1 | *founder: A and B exports* | *founder* | n/a | *founder* |
| E2 | *founder* | *founder* | n/a | *founder* |
| E3 | *founder: one per request* | *founder* | *founder: one per transaction* | *founder* |
| E4 | *founder, or "no hosted Strike bundle"* | *founder* | *founder* | *founder* |
| E5 | n/a | n/a | *founder* | *founder* |
| E6 | *founder* | *founder* | n/a | *founder* |

Account hashes: A `sub:…`, B `sub:…` (*founder*).

## Dry run with MCP Inspector

These steps can be rehearsed against a local hosted-mode instance with
[MCP Inspector](https://github.com/modelcontextprotocol/inspector). This is
not acceptance evidence: Inspector sends the calls it is told to, so no model
chooses them.

1. Serve the hosted bundles in `oidc` mode with a `[store]` under
   `fluent demo record`, with a copy of `hosted.toml` that listens on
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
4. Run [E5](#e5-decode-check) on the prepared CBOR.
