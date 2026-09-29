//! The reviewable summary of a transaction, decoded from its CBOR.
//!
//! [`decode`] reads only the CBOR it is given, never the arguments that
//! produced it, so its [`Summary`] is an independent check of what a resolver
//! built. It is a pure function: no I/O, no clock, no configuration.
//!
//! The CBOR must be one complete Conway-era transaction (`[body, witnesses,
//! is_valid, auxiliary_data]`) with nothing after it. The transaction hash is
//! the Blake2b-256 of the body bytes exactly as they appear in the CBOR.

use pallas_addresses::Address;
use pallas_codec::minicbor;
use pallas_crypto::hash::Hasher;
use pallas_primitives::conway::{self, Tx};
use pallas_primitives::{alonzo, babbage};
use serde::{Deserialize, Serialize};

use crate::envelope::{
    AssetAmount, InputSummary, MintAmount, OutputSummary, TransactionSummary, ValidityRange,
};
use crate::error::FluentError;

/// What [`decode`] reads from a transaction's CBOR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    /// The Blake2b-256 of the transaction body bytes, hex encoded.
    pub tx_hash: String,
    /// The decoded view a person reviews before signing.
    pub transaction: TransactionSummary,
}

/// Why a transaction's CBOR could not be decoded. Carried as the source of a
/// [`FluentError::Internal`], so callers never see it.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// The text is not hex.
    #[error("transaction CBOR is not hex: {0}")]
    Hex(#[from] hex::FromHexError),
    /// The bytes are not a Conway-era transaction.
    #[error("not a Conway-era transaction: {0}")]
    Cbor(#[from] minicbor::decode::Error),
    /// Bytes follow the transaction.
    #[error("{0} bytes follow the transaction")]
    TrailingBytes(usize),
    /// An output address cannot be decoded.
    #[error("output {index} has an undecodable address: {source}")]
    Address {
        /// The output's position in the transaction.
        index: usize,
        /// The address error.
        #[source]
        source: pallas_addresses::Error,
    },
}

/// Decodes hex-encoded transaction CBOR into its [`Summary`].
///
/// Inputs are listed as `<tx_hash>#<index>` in CBOR order; outputs in CBOR
/// order with bech32 addresses (base58 for Byron), lovelace and native
/// assets; then the fee, the mint (negative amounts are burns), the validity
/// interval and the required signers. Native assets and mints are ordered by
/// policy id, then asset name.
///
/// Fails with [`FluentError::Internal`] when `tx_hex` is not hex, is not one
/// complete Conway-era transaction, or has an output whose address cannot be
/// decoded.
pub fn decode(tx_hex: &str) -> Result<Summary, FluentError> {
    decode_hex(tx_hex).map_err(FluentError::internal)
}

/// [`decode`], saying why the CBOR cannot be decoded: for CBOR a person
/// supplied, such as `fluent verify`'s, rather than a resolver's.
pub fn try_decode(tx_hex: &str) -> Result<Summary, DecodeError> {
    decode_hex(tx_hex)
}

fn decode_hex(tx_hex: &str) -> Result<Summary, DecodeError> {
    let bytes = hex::decode(tx_hex)?;
    let mut decoder = minicbor::Decoder::new(&bytes);
    let tx: Tx = decoder.decode()?;
    let trailing = bytes.len() - decoder.position();
    if trailing > 0 {
        return Err(DecodeError::TrailingBytes(trailing));
    }

    let body = &tx.transaction_body;
    let tx_hash = hex::encode(Hasher::<256>::hash(body.raw_cbor()));

    let inputs = body
        .inputs
        .iter()
        .map(|input| InputSummary {
            utxo_ref: format!("{}#{}", input.transaction_id, input.index),
        })
        .collect();

    let outputs = body
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| output_summary(index, output))
        .collect::<Result<_, _>>()?;

    let mint = body
        .mint
        .iter()
        .flatten()
        .flat_map(|(policy, assets)| {
            assets.iter().map(move |(name, amount)| MintAmount {
                policy_id: policy.to_string(),
                asset_name_hex: hex::encode(name.as_slice()),
                amount: i64::from(amount),
            })
        })
        .collect();

    let required_signers = body
        .required_signers
        .iter()
        .flat_map(|signers| signers.iter())
        .map(ToString::to_string)
        .collect();

    Ok(Summary {
        tx_hash,
        transaction: TransactionSummary {
            inputs,
            outputs,
            fee_lovelace: body.fee,
            mint,
            validity: ValidityRange {
                from_slot: body.validity_interval_start,
                until_slot: body.ttl,
            },
            required_signers,
        },
    })
}

fn output_summary(
    index: usize,
    output: &conway::TransactionOutput<'_>,
) -> Result<OutputSummary, DecodeError> {
    let (address, lovelace, assets) = match output {
        babbage::GenTransactionOutput::Legacy(legacy) => {
            let (lovelace, assets) = match &legacy.amount {
                alonzo::Value::Coin(coin) => (*coin, Vec::new()),
                alonzo::Value::Multiasset(coin, multiasset) => {
                    (*coin, asset_amounts(multiasset, |amount| *amount))
                }
            };
            (&legacy.address, lovelace, assets)
        }
        babbage::GenTransactionOutput::PostAlonzo(post) => {
            let (lovelace, assets) = match &post.value {
                conway::Value::Coin(coin) => (*coin, Vec::new()),
                conway::Value::Multiasset(coin, multiasset) => {
                    (*coin, asset_amounts(multiasset, |amount| u64::from(amount)))
                }
            };
            (&post.address, lovelace, assets)
        }
    };
    Ok(OutputSummary {
        address: address_text(address.as_slice())
            .map_err(|source| DecodeError::Address { index, source })?,
        lovelace,
        assets,
    })
}

fn asset_amounts<A>(
    multiasset: &conway::Multiasset<A>,
    amount: impl Fn(&A) -> u64,
) -> Vec<AssetAmount> {
    multiasset
        .iter()
        .flat_map(|(policy, assets)| {
            assets.iter().map(|(name, value)| AssetAmount {
                policy_id: policy.to_string(),
                asset_name_hex: hex::encode(name.as_slice()),
                amount: amount(value),
            })
        })
        .collect()
}

/// An output address as people read it: bech32, or base58 for Byron.
fn address_text(bytes: &[u8]) -> Result<String, pallas_addresses::Error> {
    match Address::from_bytes(bytes)? {
        Address::Byron(byron) => Ok(byron.to_base58()),
        address => address.to_bech32(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn rejects_text_that_is_not_a_transaction() {
        for text in ["zz", "", "00", "a0"] {
            let err = decode(text).unwrap_err();
            assert_eq!(err.code(), ErrorCode::Internal, "{text}");
            assert_eq!(err.details(), None);
        }
    }
}
