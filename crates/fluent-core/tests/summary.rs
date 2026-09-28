//! `summary::decode` against real Cardano preprod transactions.
//!
//! Each `tests/fixtures/tx/{name}.hex` is the CBOR of a transaction on the
//! preprod chain, exactly as Koios `tx_cbor` returned it. The matching
//! `{name}.summary.json` was written from the explorer's view of the same
//! transaction (Koios `tx_info`, backed by cardano-db-sync: inputs, outputs
//! with bech32 addresses and assets, fee, mint, validity interval), never from
//! the decoder, and compared field by field. `tx_info` does not list required
//! signers: the one key hash in `script-preprod` was checked to be the payment
//! credential `tx_info` reports for that transaction's collateral input and
//! change address.
//!
//! | Fixture | Transaction | Covers |
//! | --- | --- | --- |
//! | `transfer-preprod` | `b2698db1…8f73` | an ADA transfer: two inputs, a payment and change |
//! | `mint-preprod` | `d6ebdcbd…0770` | a mint, native assets in outputs, a TTL |
//! | `script-preprod` | `c818eef5…f0b2` | a script spend with collateral, a script address, a validity window and a required signer |
//!
//! The transaction hash in each summary is the explorer's; the decoder
//! recomputes it from the body bytes.

use std::fs;
use std::path::PathBuf;

use fluent_core::ErrorCode;
use fluent_core::summary::{self, Summary};

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tx")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn check(name: &str) -> Summary {
    let hex = fixture(&format!("{name}.hex"));
    let expected: Summary =
        serde_json::from_str(&fixture(&format!("{name}.summary.json"))).expect("summary JSON");
    let decoded = summary::decode(hex.trim()).unwrap_or_else(|err| panic!("{name}: {err:?}"));
    assert_eq!(decoded, expected, "{name}");
    decoded
}

#[test]
fn decodes_a_preprod_transfer() {
    let decoded = check("transfer-preprod");
    assert_eq!(
        decoded.tx_hash,
        "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73"
    );
    let tx = decoded.transaction;
    assert_eq!(tx.inputs.len(), 2);
    assert_eq!(tx.outputs[0].lovelace, 3_000_000);
    assert_eq!(tx.fee_lovelace, 169_725);
    assert!(tx.mint.is_empty());
    assert_eq!(tx.validity.from_slot, None);
    assert_eq!(tx.validity.until_slot, None);
}

#[test]
fn decodes_a_preprod_mint() {
    let tx = check("mint-preprod").transaction;
    assert_eq!(tx.mint.len(), 1);
    assert_eq!(tx.mint[0].amount, 1);
    assert_eq!(tx.validity.until_slot, Some(134_875_213));
    assert!(tx.outputs.iter().any(|output| output.assets.len() > 1));
}

#[test]
fn decodes_a_preprod_script_spend() {
    let tx = check("script-preprod").transaction;
    assert_eq!(tx.validity.from_slot, Some(134_872_999));
    assert_eq!(tx.validity.until_slot, Some(134_873_899));
    assert!(tx.outputs[0].address.starts_with("addr_test1w"));
    assert_eq!(
        tx.required_signers,
        ["f0a26fc170ad82b64a3d43dede08e78ea6e2028b5101058d0263a80a"]
    );
}

#[test]
fn rejects_truncated_or_padded_cbor() {
    let hex = fixture("transfer-preprod.hex");
    let hex = hex.trim();
    for text in [&hex[..hex.len() - 2], &format!("{hex}00")] {
        let err = summary::decode(text).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Internal);
    }
}
