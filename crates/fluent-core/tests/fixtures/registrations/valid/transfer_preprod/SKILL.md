---
name: transfer-preprod
description: Fixture for the SDK transfer test vector. Use only in Tx3 Fluent loader tests.
license: Apache-2.0
protocol: unknown/unknown:0.0.1
tii_digest: sha256:8d5d715f300e618373b588af96f0cfb9b0a3904b3a34fc0038f72ebb1695da31
network: preprod
revision: 1
dependencies:
  - id: sender_address
    description: The bech32 preprod address of the user sending ADA.
    required_for: [transfer]
---
# transfer-preprod

A loader fixture, not a reviewed companion skill.
