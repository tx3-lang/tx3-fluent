//! Decodes a transaction's CBOR with pallas, apart from the `fluent`
//! runtime.
//!
//! This is deliberately not `fluent_core::summary`: [`verify`](crate::verify)
//! is meant to check the envelope a resolver produced, so it reads the CBOR
//! with its own decoder rather than the code that builds the envelope's
//! summary. Only the output shape, [`TransactionSummary`], is shared, so
//! `cargo xtask verify` prints the same JSON a tool result carries.
//!
//! The CBOR must be one complete Conway-era transaction with nothing after
//! it. The hash is the Blake2b-256 of the body bytes as they appear in the
//! CBOR.

use fluent_core::envelope::{
    AssetAmount, InputSummary, MintAmount, OutputSummary, TransactionSummary, ValidityRange,
};
use pallas_addresses::Address;
use pallas_codec::minicbor;
use pallas_crypto::hash::Hasher;
use pallas_primitives::alonzo;
use pallas_primitives::babbage::GenTransactionOutput;
use pallas_primitives::conway::{self, Multiasset, Tx};

/// Why a transaction's CBOR cannot be decoded.
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

/// Decodes hex-encoded transaction CBOR into its hash and summary: inputs as
/// `<tx_hash>#<index>` and outputs in CBOR order, addresses in bech32 (base58
/// for Byron), native assets and mints ordered by policy id, then asset name.
pub fn decode(tx_hex: &str) -> Result<(String, TransactionSummary), DecodeError> {
    let bytes = hex::decode(tx_hex)?;
    let mut decoder = minicbor::Decoder::new(&bytes);
    let tx: Tx = decoder.decode()?;
    let trailing = bytes.len() - decoder.position();
    if trailing > 0 {
        return Err(DecodeError::TrailingBytes(trailing));
    }

    let body = &tx.transaction_body;
    let tx_hash = hex::encode(Hasher::<256>::hash(body.raw_cbor()));
    let outputs = body
        .outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            output_summary(output).map_err(|source| DecodeError::Address { index, source })
        })
        .collect::<Result<_, _>>()?;
    let summary = TransactionSummary {
        inputs: body
            .inputs
            .iter()
            .map(|input| InputSummary {
                utxo_ref: format!("{}#{}", input.transaction_id, input.index),
            })
            .collect(),
        outputs,
        fee_lovelace: body.fee,
        mint: body
            .mint
            .iter()
            .flat_map(|mint| mint.iter())
            .flat_map(|(policy, assets)| {
                assets.iter().map(move |(name, amount)| MintAmount {
                    policy_id: policy.to_string(),
                    asset_name_hex: hex::encode(name.as_slice()),
                    amount: i64::from(amount),
                })
            })
            .collect(),
        validity: ValidityRange {
            from_slot: body.validity_interval_start,
            until_slot: body.ttl,
        },
        required_signers: body
            .required_signers
            .iter()
            .flat_map(|signers| signers.iter())
            .map(ToString::to_string)
            .collect(),
    };
    Ok((tx_hash, summary))
}

fn output_summary(
    output: &conway::TransactionOutput<'_>,
) -> Result<OutputSummary, pallas_addresses::Error> {
    let (address, lovelace, assets) = match output {
        GenTransactionOutput::Legacy(legacy) => match &legacy.amount {
            alonzo::Value::Coin(coin) => (&legacy.address, *coin, Vec::new()),
            alonzo::Value::Multiasset(coin, assets) => {
                (&legacy.address, *coin, amounts(assets, |a| *a))
            }
        },
        GenTransactionOutput::PostAlonzo(post) => match &post.value {
            conway::Value::Coin(coin) => (&post.address, *coin, Vec::new()),
            conway::Value::Multiasset(coin, assets) => {
                (&post.address, *coin, amounts(assets, |a| u64::from(a)))
            }
        },
    };
    let address = match Address::from_bytes(address.as_slice())? {
        Address::Byron(byron) => byron.to_base58(),
        other => other.to_bech32()?,
    };
    Ok(OutputSummary {
        address,
        lovelace,
        assets,
    })
}

fn amounts<A>(assets: &Multiasset<A>, amount: impl Fn(&A) -> u64) -> Vec<AssetAmount> {
    assets
        .iter()
        .flat_map(|(policy, names)| {
            names.iter().map(|(name, value)| AssetAmount {
                policy_id: policy.to_string(),
                asset_name_hex: hex::encode(name.as_slice()),
                amount: amount(value),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_text_that_is_not_one_transaction() {
        for text in ["zz", "", "00", "a0"] {
            assert!(decode(text).is_err(), "{text}");
        }
    }
}
