//! `verify::verify` against the real transactions in
//! `fluent-core/tests/fixtures/tx/` (see fluent-core's `tests/summary.rs`): a
//! matching expectation, a wrong amount, a wrong address, a wrong network,
//! native assets and required signers.

use std::fs;
use std::path::PathBuf;

use fluent_core::ErrorCode;
use fluent_core::registration::Network;
use xtask::verify::{CheckKind, Expectations, OutputExpectation, Verdict, verify};

/// The transfer's receiver, paid 3 ADA, and its sender, paid the change.
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";

/// The mint's second output: 1,172,320 lovelace and the minted `FIELD-000020`.
const MINTER: &str = "addr_test1qpdmfr7ltjmdm6ef8ygh5224scsvktlt5lm7fpd3yre2xktv66cufzyerwhaw9xety2f756chgdj0svt6r5syr00fy3sh5fe9y";
const FIELD_POLICY: &str = "81c43d73f7ea776126f4034542dd905a7b52f31d6f4ccf6260dd2e4b";

/// The script spend's one required signer.
const SCRIPT_SIGNER: &str = "f0a26fc170ad82b64a3d43dede08e78ea6e2028b5101058d0263a80a";

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/fluent-core/tests/fixtures/tx")
        .join(format!("{name}.hex"));
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .trim()
        .to_string()
}

fn output(text: &str) -> OutputExpectation {
    text.parse().unwrap_or_else(|err| panic!("{text}: {err:?}"))
}

fn expect(outputs: &[&str], network: Network, signers: &[&str]) -> Expectations {
    Expectations {
        outputs: outputs.iter().map(|o| output(o)).collect(),
        network,
        signers: signers.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn a_matching_transfer_passes() {
    let verification = verify(
        &fixture("transfer-preprod"),
        &expect(&[&format!("{RECEIVER}=3000000")], Network::Preprod, &[]),
    )
    .expect("verify");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(verification.tx_hash, TRANSFER_HASH);
    assert_eq!(verification.summary.outputs.len(), 2);
    let kinds: Vec<CheckKind> = verification.checks.iter().map(|c| c.check).collect();
    assert_eq!(kinds, [CheckKind::Output, CheckKind::Network]);
    assert_eq!(verification.checks[0].detail, "output 0");
    assert!(verification.checks.iter().all(|c| c.passed));
}

#[test]
fn every_expected_output_must_match_its_own_output() {
    let both = expect(
        &[&format!("{SENDER}=2852207"), &format!("{RECEIVER}=3000000")],
        Network::Preprod,
        &[],
    );
    let verification = verify(&fixture("transfer-preprod"), &both).expect("verify");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(verification.checks[0].detail, "output 1");

    let twice = expect(
        &[
            &format!("{RECEIVER}=3000000"),
            &format!("{RECEIVER}=3000000"),
        ],
        Network::Preprod,
        &[],
    );
    let verification = verify(&fixture("transfer-preprod"), &twice).expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    assert!(verification.checks[0].passed);
    assert!(!verification.checks[1].passed);
}

#[test]
fn a_wrong_amount_fails() {
    let verification = verify(
        &fixture("transfer-preprod"),
        &expect(&[&format!("{RECEIVER}=3000001")], Network::Preprod, &[]),
    )
    .expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    let check = &verification.checks[0];
    assert!(!check.passed);
    assert_eq!(
        check.detail,
        "no unmatched output carries exactly this value; output 0 carries 3000000"
    );
    // The network still matches: only the output failed.
    assert!(verification.checks[1].passed);
}

#[test]
fn a_wrong_address_fails() {
    // The minter's address is a valid preprod address the transfer never pays.
    let verification = verify(
        &fixture("transfer-preprod"),
        &expect(&[&format!("{MINTER}=3000000")], Network::Preprod, &[]),
    )
    .expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    assert!(!verification.checks[0].passed);
    assert_eq!(verification.checks[0].detail, "no output pays this address");
}

#[test]
fn a_wrong_network_fails() {
    let verification = verify(
        &fixture("transfer-preprod"),
        &expect(&[&format!("{RECEIVER}=3000000")], Network::Mainnet, &[]),
    )
    .expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    assert!(verification.checks[0].passed, "the output itself matches");
    let network = &verification.checks[1];
    assert_eq!(network.check, CheckKind::Network);
    assert_eq!(network.expected, "mainnet");
    assert!(!network.passed);
    assert_eq!(
        network.detail,
        "output 0 is on a testnet (preprod or preview); output 1 is on a testnet (preprod or preview)"
    );

    // Addresses cannot tell the testnets apart, so preview passes as well.
    let preview = verify(
        &fixture("transfer-preprod"),
        &expect(&[&format!("{RECEIVER}=3000000")], Network::Preview, &[]),
    )
    .expect("verify");
    assert_eq!(preview.verdict, Verdict::Match);
}

#[test]
fn native_assets_must_match_exactly() {
    let hex = fixture("mint-preprod");
    let minted = format!("{MINTER}=1172320+{FIELD_POLICY}.4649454c442d303030303230=1");
    let verification = verify(&hex, &expect(&[&minted], Network::Preprod, &[])).expect("verify");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(verification.checks[0].detail, "output 1");

    // Leaving the asset out, or getting its amount wrong, fails.
    for wrong in [
        format!("{MINTER}=1172320"),
        format!("{MINTER}=1172320+{FIELD_POLICY}.4649454c442d303030303230=2"),
    ] {
        let verification = verify(&hex, &expect(&[&wrong], Network::Preprod, &[])).expect("verify");
        assert_eq!(verification.verdict, Verdict::Mismatch, "{wrong}");
        assert!(
            verification.checks[0].detail.contains(&format!(
                "output 1 carries 1172320+{FIELD_POLICY}.4649454c442d303030303230=1"
            )),
            "{:#?}",
            verification.checks[0]
        );
    }
}

#[test]
fn expected_signers_must_be_required() {
    let hex = fixture("script-preprod");
    let change = "addr_test1qrc2ym7pwzkc9dj284paahsgu782dcsz3dgszpvdqf36szjvpmpraw365fayhrtpzpl4nulq6f9hhdkh4cdyh0tgnjxsaxg4ak=8278224016";
    let verification = verify(
        &hex,
        &expect(
            &[change],
            Network::Preprod,
            &[&SCRIPT_SIGNER.to_uppercase()],
        ),
    )
    .expect("verify");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(verification.checks[2].check, CheckKind::Signer);
    assert_eq!(verification.checks[2].expected, SCRIPT_SIGNER);

    let other = "00000000000000000000000000000000000000000000000000000000";
    let verification =
        verify(&hex, &expect(&[change], Network::Preprod, &[other])).expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    assert_eq!(
        verification.checks[2].detail,
        "not among the 1 required signers"
    );
}

#[test]
fn malformed_expectations_are_invalid_arguments() {
    for text in [
        "no-equals-sign",
        "not-an-address=5",
        &format!("{RECEIVER}=lots"),
        &format!("{RECEIVER}=5+{FIELD_POLICY}=1"),
        &format!("{RECEIVER}=5+abcd.00=1"),
        &format!("{RECEIVER}=5+{FIELD_POLICY}.0=1"),
        &format!("{RECEIVER}=5+{FIELD_POLICY}.00=1+{FIELD_POLICY}.00=2"),
    ] {
        let err = text.parse::<OutputExpectation>().expect_err(text);
        assert_eq!(err.code(), ErrorCode::InvalidArguments, "{text}");
        assert!(
            !err.message().contains(RECEIVER),
            "{text}: {}",
            err.message()
        );
    }

    let transfer = fixture("transfer-preprod");
    let none = Expectations {
        outputs: Vec::new(),
        network: Network::Preprod,
        signers: Vec::new(),
    };
    assert_eq!(
        verify(&transfer, &none).expect_err("no outputs").code(),
        ErrorCode::InvalidArguments
    );
    let bad_signer = expect(
        &[&format!("{RECEIVER}=3000000")],
        Network::Preprod,
        &["abc"],
    );
    assert_eq!(
        verify(&transfer, &bad_signer)
            .expect_err("bad signer")
            .code(),
        ErrorCode::InvalidArguments
    );
    let err = verify(
        "84a0",
        &expect(&[&format!("{RECEIVER}=1")], Network::Preprod, &[]),
    )
    .expect_err("truncated CBOR");
    assert_eq!(err.code(), ErrorCode::InvalidArguments);
    assert!(
        err.message()
            .starts_with("invalid arguments: the transaction cannot be decoded")
    );
}

#[test]
fn the_expectation_prints_as_written() {
    let text = format!("{MINTER}=1172320+{FIELD_POLICY}.4649454c442d303030303230=1");
    assert_eq!(output(&text).to_string(), text);
    // Hex addresses print as bech32, the summary's spelling.
    let hex = fluent_core::address::inspect(RECEIVER)
        .expect("inspect")
        .hex;
    assert_eq!(
        output(&format!("{hex}=1")).to_string(),
        format!("{RECEIVER}=1")
    );
}

#[test]
fn a_mainnet_strike_withdrawal_matches_on_mainnet_only() {
    // `22d2ae9c…4875`, from Koios `tx_cbor`: the staker gets 232,601,194 base
    // units of STRIKE back, and the owner's key hash must sign.
    let hex = fixture("strike-withdraw-mainnet");
    let staker = "addr1q87emegun3cqmrzup7ddenca4mj85cgejgh74cj0zpm3c435zrq0euvqekgale2ak7rza3lafk4gq93lxm2w2gk2mmcs0uj0c2=2184169+f13ac4d66b3ee19a6aa0f2a22298737bd907cc95121662fc971b5275.535452494b45=232601194";
    let owner = "fd9de51c9c700d8c5c0f9adccf1daee47a6119922feae24f10771c56";

    let verification =
        verify(&hex, &expect(&[staker], Network::Mainnet, &[owner])).expect("verify");
    assert_eq!(verification.verdict, Verdict::Match, "{verification:#?}");
    assert_eq!(
        verification.tx_hash,
        "22d2ae9c4d076acac2779bfcc1a2a327b788bfafe9022a7c5e81c8671f824875"
    );
    assert_eq!(verification.summary.fee_lovelace, 315_831);

    let verification =
        verify(&hex, &expect(&[staker], Network::Preprod, &[owner])).expect("verify");
    assert_eq!(verification.verdict, Verdict::Mismatch);
    assert_eq!(verification.checks[1].detail, "output 0 is on mainnet");
}
