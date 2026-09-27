//! Result envelopes returned to callers.
//!
//! [`PreparedTransaction`] is the only successful result of preparing a
//! transaction. Its wire shape is a contract shared by every transport, and its
//! [`JsonSchema`] is advertised as the MCP tool output schema. The fields that
//! must never vary — `status`, `signed`, `submitted` and `next_steps` — are
//! marker types, so a value with any other content cannot be built or parsed.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The fixed sentence carried in [`PreparedTransaction::next_steps`].
pub const NEXT_STEPS: &str = "This transaction is unsigned and has not been submitted. \
Import the CBOR into a wallet that can sign unsigned transactions, review the summary \
there, then sign and submit.";

/// An unsigned, unsubmitted transaction ready for a wallet to review and sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedTransaction {
    /// Always `"prepared_unsigned"`.
    pub status: PreparedStatus,
    /// Always `false`: Fluent never signs.
    pub signed: AlwaysFalse,
    /// Always `false`: Fluent never submits.
    pub submitted: AlwaysFalse,
    /// The protocol registration the transaction was prepared from.
    pub protocol: ProtocolRef,
    /// The name of the protocol transaction that was prepared.
    pub transaction: String,
    /// The network the transaction was resolved against.
    pub network: String,
    /// The transaction hash, hex encoded.
    pub tx_hash: String,
    /// The unsigned transaction body in CBOR, hex encoded.
    pub unsigned_tx_cbor_hex: String,
    /// A decoded summary for review before signing.
    pub summary: TransactionSummary,
    /// Fixed instructions to sign and submit with a wallet (`NEXT_STEPS`).
    pub next_steps: NextSteps,
}

impl PreparedTransaction {
    /// Builds an envelope, filling the fixed fields.
    pub fn new(
        protocol: ProtocolRef,
        transaction: impl Into<String>,
        network: impl Into<String>,
        tx_hash: impl Into<String>,
        unsigned_tx_cbor_hex: impl Into<String>,
        summary: TransactionSummary,
    ) -> Self {
        PreparedTransaction {
            status: PreparedStatus::PreparedUnsigned,
            signed: AlwaysFalse,
            submitted: AlwaysFalse,
            protocol,
            transaction: transaction.into(),
            network: network.into(),
            tx_hash: tx_hash.into(),
            unsigned_tx_cbor_hex: unsigned_tx_cbor_hex.into(),
            summary,
            next_steps: NextSteps,
        }
    }
}

/// The status of a [`PreparedTransaction`]; it has exactly one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreparedStatus {
    /// Prepared, not signed, not submitted.
    PreparedUnsigned,
}

/// A boolean that is `false` on the wire and rejects `true` when parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlwaysFalse;

impl Serialize for AlwaysFalse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for AlwaysFalse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Err(serde::de::Error::custom("expected `false`"))
        } else {
            Ok(AlwaysFalse)
        }
    }
}

impl JsonSchema for AlwaysFalse {
    fn schema_name() -> Cow<'static, str> {
        "AlwaysFalse".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "boolean", "const": false })
    }
}

/// The [`NEXT_STEPS`] sentence; rejects any other text when parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NextSteps;

impl Serialize for NextSteps {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(NEXT_STEPS)
    }
}

impl<'de> Deserialize<'de> for NextSteps {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = Cow::<str>::deserialize(deserializer)?;
        if text == NEXT_STEPS {
            Ok(NextSteps)
        } else {
            Err(serde::de::Error::custom(
                "unexpected `next_steps` text for a prepared transaction",
            ))
        }
    }
}

impl JsonSchema for NextSteps {
    fn schema_name() -> Cow<'static, str> {
        "NextSteps".into()
    }

    fn inline_schema() -> bool {
        true
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "const": NEXT_STEPS })
    }
}

/// Identifies the protocol registration a transaction was prepared from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProtocolRef {
    /// The registry scope that publishes the protocol.
    pub scope: String,
    /// The protocol name within its scope.
    pub name: String,
    /// The protocol version.
    pub version: String,
    /// The slug of the registration that served the request.
    pub registration_slug: String,
    /// The registration revision that served the request.
    pub registration_revision: String,
    /// The digest of the TII artifact used, for example `sha256:<hex>`.
    pub tii_digest: String,
}

/// A decoded view of a transaction for review before signing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransactionSummary {
    /// The UTxOs the transaction spends.
    pub inputs: Vec<InputSummary>,
    /// The outputs the transaction creates.
    pub outputs: Vec<OutputSummary>,
    /// The transaction fee in lovelace.
    pub fee_lovelace: u64,
    /// Assets minted (positive) or burned (negative).
    pub mint: Vec<MintAmount>,
    /// The slot range in which the transaction is valid.
    pub validity: ValidityRange,
    /// Key hashes that must sign, hex encoded.
    pub required_signers: Vec<String>,
}

/// A spent UTxO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputSummary {
    /// The UTxO reference as `<tx_hash>#<index>`.
    #[serde(rename = "ref")]
    pub utxo_ref: String,
}

/// A created output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputSummary {
    /// The receiving address, bech32 encoded.
    pub address: String,
    /// The lovelace the output holds.
    pub lovelace: u64,
    /// The native assets the output holds.
    pub assets: Vec<AssetAmount>,
}

/// A quantity of one native asset held by an output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssetAmount {
    /// The minting policy hash, hex encoded.
    pub policy_id: String,
    /// The asset name, hex encoded.
    pub asset_name_hex: String,
    /// The quantity held.
    pub amount: u64,
}

/// A quantity of one native asset minted or burned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MintAmount {
    /// The minting policy hash, hex encoded.
    pub policy_id: String,
    /// The asset name, hex encoded.
    pub asset_name_hex: String,
    /// The quantity minted (positive) or burned (negative).
    pub amount: i64,
}

/// The slot range in which a transaction is valid; absent bounds are open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidityRange {
    /// The first slot in which the transaction is valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_slot: Option<u64>,
    /// The slot from which the transaction is no longer valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_slot: Option<u64>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn sample() -> PreparedTransaction {
        PreparedTransaction::new(
            ProtocolRef {
                scope: "acme".into(),
                name: "swap".into(),
                version: "1.2.0".into(),
                registration_slug: "acme-swap".into(),
                registration_revision: "4f1c2a9".into(),
                tii_digest: "sha256:ab12".into(),
            },
            "swap",
            "preprod",
            "d1f0",
            "84a4",
            TransactionSummary {
                inputs: vec![InputSummary {
                    utxo_ref: "aa11#0".into(),
                }],
                outputs: vec![OutputSummary {
                    address: "addr_test1qexample".into(),
                    lovelace: 2_000_000,
                    assets: vec![AssetAmount {
                        policy_id: "cafe".into(),
                        asset_name_hex: "74657374".into(),
                        amount: 5,
                    }],
                }],
                fee_lovelace: 170_000,
                mint: vec![MintAmount {
                    policy_id: "cafe".into(),
                    asset_name_hex: "74657374".into(),
                    amount: -1,
                }],
                validity: ValidityRange {
                    from_slot: None,
                    until_slot: Some(1_000),
                },
                required_signers: vec!["beef".into()],
            },
        )
    }

    fn expected_json() -> Value {
        json!({
            "status": "prepared_unsigned",
            "signed": false,
            "submitted": false,
            "protocol": {
                "scope": "acme",
                "name": "swap",
                "version": "1.2.0",
                "registration_slug": "acme-swap",
                "registration_revision": "4f1c2a9",
                "tii_digest": "sha256:ab12"
            },
            "transaction": "swap",
            "network": "preprod",
            "tx_hash": "d1f0",
            "unsigned_tx_cbor_hex": "84a4",
            "summary": {
                "inputs": [{ "ref": "aa11#0" }],
                "outputs": [{
                    "address": "addr_test1qexample",
                    "lovelace": 2_000_000,
                    "assets": [{ "policy_id": "cafe", "asset_name_hex": "74657374", "amount": 5 }]
                }],
                "fee_lovelace": 170_000,
                "mint": [{ "policy_id": "cafe", "asset_name_hex": "74657374", "amount": -1 }],
                "validity": { "until_slot": 1_000 },
                "required_signers": ["beef"]
            },
            "next_steps": NEXT_STEPS
        })
    }

    #[test]
    fn serializes_with_the_contract_field_names() {
        assert_eq!(serde_json::to_value(sample()).unwrap(), expected_json());
    }

    #[test]
    fn round_trips_through_json() {
        let text = serde_json::to_string(&sample()).unwrap();
        let parsed: PreparedTransaction = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, sample());
    }

    #[test]
    fn next_steps_is_the_contract_sentence() {
        assert_eq!(
            NEXT_STEPS,
            "This transaction is unsigned and has not been submitted. Import the CBOR into a \
             wallet that can sign unsigned transactions, review the summary there, then sign \
             and submit."
        );
    }

    #[test]
    fn rejects_signed_submitted_or_altered_envelopes() {
        for (field, value) in [
            ("signed", json!(true)),
            ("submitted", json!(true)),
            ("status", json!("signed")),
            ("next_steps", json!("Submit it.")),
        ] {
            let mut doc = expected_json();
            doc[field] = value;
            assert!(
                serde_json::from_value::<PreparedTransaction>(doc).is_err(),
                "accepted altered `{field}`"
            );
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut doc = expected_json();
        doc["signature"] = json!("00");
        assert!(serde_json::from_value::<PreparedTransaction>(doc).is_err());
    }

    #[test]
    fn schema_pins_the_fixed_fields() {
        let schema = serde_json::to_value(schemars::schema_for!(PreparedTransaction)).unwrap();
        let props = &schema["properties"];
        assert_eq!(props["signed"]["const"], json!(false));
        assert_eq!(props["submitted"]["const"], json!(false));
        assert_eq!(props["next_steps"]["const"], json!(NEXT_STEPS));
        assert_eq!(schema["additionalProperties"], json!(false));
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            required,
            [
                "status",
                "signed",
                "submitted",
                "protocol",
                "transaction",
                "network",
                "tx_hash",
                "unsigned_tx_cbor_hex",
                "summary",
                "next_steps",
            ]
        );
    }
}
