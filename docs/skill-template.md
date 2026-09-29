# Consumption skill template

A consumption skill is the `SKILL.md` of a
[registration bundle](../README.md#registration-bundles). `fluent_get_skill`
returns it to an assistant before the assistant calls any of the
registration's transaction tools. It tells the assistant how to gather the
right inputs in the right units, what it must obtain outside Fluent, and what
the unsigned result means. It is written for one protocol version, one TII and
one network, and says nothing that is not true of all three.

The hosted skills live in
[`deploy/hosted/registrations/`](../deploy/hosted/registrations). A test
checks every one of them against this template.

## Frontmatter

```yaml
---
name: transfer-preprod
description: Prepares an unsigned preprod ADA payment with open-tx3/transfer; use when the user wants to send test ADA from one preprod address to another.
license: Apache-2.0
protocol: open-tx3/transfer:0.1.0
tii_digest: sha256:…
network: preprod
revision: 1
dependencies:
  - id: wallet-address
    description: The user's own bech32 preprod address, copied from their wallet.
    required_for: [transfer]
---
```

| Key | Rule |
| --- | --- |
| `name` | The registration slug with `_` written as `-`. |
| `description` | One sentence: what the skill prepares, ending with "; use when …" to say when to use it. |
| `license` | `Apache-2.0`. |
| `protocol` | `scope/name:version`, equal to the registration's `[protocol]`. |
| `tii_digest` | The registration's `artifact.tii_digest`. |
| `network` | The registration's `deployment.network`. |
| `revision` | The skill's own revision: 1 for the first reviewed text, then incremented on every change to the file. |
| `dependencies` | One entry per thing the assistant must obtain outside Fluent: a kebab-case `id`, a `description` saying what it is and where it comes from, and `required_for`, the TII transactions that need it. Use `[]` when there is none. |

The loader rejects a skill whose `protocol`, `tii_digest` or `network` differs
from its registration, or whose dependency names a transaction the TII does not
define.

## Body

The body starts with a `# ` title, then has exactly these seven `## ` sections,
in this order. Write to the assistant, in the imperative.

1. **Purpose** — what the protocol does, which transactions the registration
   offers (one line each, with the tool name `{slug}_{tx}`), and the network.
2. **Before you start** — call `fluent_get_skill` first (this skill), then
   which transaction tool fits which request. Confirm the network: run
   `fluent_inspect_address` on every address the user gives and check its
   `network` against the registration's; say what to do when it differs.
3. **Units** — the unit of every amount: lovelace against ADA (1 ADA =
   1,000,000 lovelace), a token's base units against its display units with
   its decimals, and how to convert. When the decimals are not confirmed, say
   so and say who confirms them.
4. **Argument preparation** — one `### ` subsection per TII transaction, titled
   with the transaction name in backticks, for example ``### `transfer` ``.
   For each argument and party: what it is, where it comes from, how to derive
   it (for example a key hash from `fluent_inspect_address`), and what to ask
   the user. Name the values the resolver or the profile supplies, so the
   assistant does not ask for them.
5. **External dependencies** — each frontmatter dependency, how the user or an
   explorer can supply it, and the exact sentence to tell the user when the
   assistant cannot obtain it.
6. **After preparation** — what the returned summary shows and proves, what the
   user must still check in the wallet before signing, and that nothing was
   signed or submitted.
7. **Do not** — at least: never invent a value, never claim the transaction is
   complete, signed or submitted, and never choose a network silently.

A transaction subsection may add `#### ` headings of its own; no other `## `
heading is allowed.

## Review

Every skill is reviewed by the founder at the pull request that adds or changes
it; the merge is that review. Bump `revision` in the same pull request.
