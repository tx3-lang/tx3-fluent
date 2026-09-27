---
name: strike-staking-mainnet
description: Fixture for the Strike staking TII on mainnet. Use only in Tx3 Fluent loader tests.
license: Apache-2.0
protocol: strike-finance/strike-staking:0.1.0
tii_digest: sha256:8da7d6658f6083325a644b7e1c6af6dc57494a7baf1addd7ed624499a2064bd3
network: mainnet
revision: 1
dependencies:
  - id: strike_balance
    description: The STRIKE balance of the staker, read from a wallet or explorer.
    required_for: [stake, add_stake]
---
# strike-staking-mainnet

A loader fixture, not a reviewed companion skill.
