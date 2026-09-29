---
name: transfer-preprod
description: Prepares an unsigned preprod ADA payment from one address to another with open-tx3/transfer; use when the user wants to send test ADA on the preprod testnet.
license: Apache-2.0
protocol: open-tx3/transfer:0.1.0
tii_digest: sha256:fcc527ea7165969f472870772b64cdbe9b4ad59ad27231790bc484de79983014
network: preprod
revision: 1
dependencies:
  - id: wallet-address
    description: The user's own bech32 preprod address (addr_test1…), copied from their wallet. Fluent has no wallet connection and cannot look it up.
    required_for: [transfer]
---
# Transfer on preprod

## Purpose

`open-tx3/transfer` is the simplest Cardano payment: it moves ADA from a
sender address to a receiver address. The receiver gets exactly the amount
asked for; the sender pays the network fee and gets the rest of the spent
UTxOs back as change. No scripts, datums or tokens are involved.

This registration offers one transaction on the **preprod testnet** only:

- `transfer`, tool `transfer_preprod_transfer`: send `quantity` lovelace from
  `sender` to `receiver`.

Preprod ADA is test ADA with no monetary value. The tool prepares an unsigned
transaction; it never signs or submits it.

## Before you start

1. You are reading this skill through `fluent_get_skill`; read it to the end
   before calling `transfer_preprod_transfer`.
2. Use this registration only when the user wants to move ADA on preprod. If
   they mean real ADA on mainnet, or the preview testnet, say this
   registration only prepares preprod transactions and stop; do not reuse it
   for another network.
3. Run `fluent_inspect_address` on every address the user gives you.
   - `network` must be `preprod_or_preview`. A preprod address and a preview
     address look the same (both start with `addr_test1`), so the report
     cannot tell them apart: ask the user to confirm the address is a preprod
     address when you have not already confirmed it.
   - If `network` is `mainnet` (an `addr1…` address), stop and say: "That is a
     mainnet address. This tool only prepares preprod test transactions;
     please give me a preprod address (it starts with addr_test1)."
   - If `kind` is `stake` (a `stake_test1…` address), it cannot hold or send
     ADA: ask for the payment address instead.
   - Pass the address to the tool as the user gave it, or use the report's
     `bech32` field when the user gave hex.

## Units

- `quantity` is in **lovelace**, a whole number: 1 ADA = 1,000,000 lovelace.
  Convert what the user says: "5 ADA" is `5000000`, "2.5 ADA" is `2500000`.
  Lovelace has no fractions, so ADA amounts have at most six decimal places;
  if the user gives more, ask which amount they mean.
- Repeat the amount back in both units before preparing, for example "5 ADA
  (5,000,000 lovelace)", so a unit mistake is caught before the wallet.
- The fee is also in lovelace. You do not choose it; it appears in the
  summary as `fee_lovelace`.
- The receiver's output must meet the ledger's minimum UTxO value, roughly
  1 ADA for a plain ADA output. Amounts below that fail.

## Argument preparation

### `transfer`

- `quantity` (integer, required): the amount the receiver gets, in lovelace.
  Ask the user how much to send; convert from ADA as described in Units.
  Never pick an amount yourself.
- `sender` (party, required): the address that pays. It must be the user's
  own preprod address, because only its owner can sign. Ask the user for it
  (dependency `wallet-address`). It must hold `quantity` plus the fee, and
  the fee is usually well under 1 ADA for this transaction.
- `receiver` (party, required): the address that receives `quantity`. Ask the
  user for it. It must be a preprod address; check it with
  `fluent_inspect_address` as described in Before you start. Sending to the
  same address as `sender` is allowed and only costs the fee.

The protocol has no environment values and nothing else to supply: the
resolver selects the sender's UTxOs, computes the fee and builds the change
output. Native tokens held in the spent UTxOs stay in the change output; this
protocol cannot send tokens.

## External dependencies

- `wallet-address`: the user's own preprod address, which becomes `sender`.
  Fluent cannot see the user's wallet, and you must not guess an address. The
  user copies it from their wallet's receive screen. When the user cannot give
  it, say: "I can't see your wallet, so I need your preprod address to
  prepare the payment. You can copy it from your wallet's receive screen; it
  starts with addr_test1."

You also cannot see the sender's balance. If preparation fails with
`insufficient_funds` or `input_not_resolved`, say: "The sender address
doesn't have enough preprod ADA to cover the amount plus the fee, or its
funds aren't visible to the resolver yet. You can get test ADA from the
Cardano testnet faucet and try again."

## After preparation

The tool returns an unsigned transaction (`unsigned_tx_cbor_hex`), its
`tx_hash`, and a `summary` decoded from those exact bytes after they were
resolved against preprod. Tell the user what the summary shows:

- `outputs`: one output to the receiver holding exactly `quantity` lovelace,
  and a change output back to the sender (which also carries any tokens from
  the spent UTxOs).
- `inputs`: the sender's UTxOs being spent.
- `fee_lovelace`: the network fee, paid by the sender.
- `mint` is empty: nothing is minted or burned.

The summary proves what these bytes would do if signed and submitted now. It
does not prove the inputs are still unspent when the user signs, or that the
receiver address is the one the user intended. Tell the user to check in
their wallet, before signing, that the wallet is on preprod, the receiver
address and amount match what they asked for, and the fee is acceptable.

Say plainly that nothing has been signed or submitted: the user must import
the CBOR into a wallet that can sign unsigned transactions, review it there,
then sign and submit. If they do not, no ADA moves.

## Do not

- Do not invent or guess an address, an amount or any other value; ask the
  user.
- Do not say the payment was sent, completed, signed or submitted. You
  prepared an unsigned transaction; only the user's wallet can sign and
  submit it.
- Do not choose a network silently or use this registration for mainnet or
  preview. If the network is unclear, ask.
- Do not describe preprod ADA as having monetary value.
- Do not ask the user for a seed phrase, mnemonic or private key, and refuse
  one if offered: Fluent never needs them.
