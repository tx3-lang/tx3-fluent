//! `fluent_core::address::inspect` against CIP-19 test vectors and real
//! addresses.

use fluent_core::address::{
    AddressKind, AddressNetwork, AddressReport, PaymentCredentialType, StakeCredentialType,
    TESTNET_NOTE, inspect,
};
use fluent_core::{ErrorCode, FluentError};
use pallas_addresses::byron::{
    AddrAttrProperty, AddrType, AddressPayload, ByronAddress, SpendingData,
};
use pallas_codec::minicbor::{self, bytes::ByteVec};
use serde_json::json;

// Credentials behind every CIP-19 test vector, from the CIP text: the key
// hashes are the Blake2b-224 digests of `addr_vk1w0l2sr2zgfm26ztc6nl9xy8ghsk5
// sh6ldwemlpmp9xylzy4dtf7st80zhd` and `stake_vk1px4j0r2fk7ux5p23shz8f3y5y2qam7
// s954rgf3lg5merqcj6aetsft99wu`; the script hash is the payload of
// `script1cda3khwqv60360rp5m7akt50m6ttapacs8rqhn5w342z7r35m37`; the pointer is
// `(2498243, 27, 3)`.
const PAYMENT_KEY_HASH: &str = "9493315cd92eb5d8c4304e67b7e16ae36d61d34502694657811a2c8e";
const STAKE_KEY_HASH: &str = "337b62cfff6403a06a3acbc34f8c46003c69fe79a3628cefa9c47251";
const SCRIPT_HASH: &str = "c37b1b5dc0669f1d3c61a6fddb2e8fde96be87b881c60bce8e8d542f";
const POINTER: (u64, u64, u64) = (2_498_243, 27, 3);

/// CIP-19 test vectors: `(address type, address)`.
const MAINNET_VECTORS: [(u8, &str); 10] = [
    (
        0,
        "addr1qx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer3n0d3vllmyqwsx5wktcd8cc3sq835lu7drv2xwl2wywfgse35a3x",
    ),
    (
        1,
        "addr1z8phkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gten0d3vllmyqwsx5wktcd8cc3sq835lu7drv2xwl2wywfgs9yc0hh",
    ),
    (
        2,
        "addr1yx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerkr0vd4msrxnuwnccdxlhdjar77j6lg0wypcc9uar5d2shs2z78ve",
    ),
    (
        3,
        "addr1x8phkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gt7r0vd4msrxnuwnccdxlhdjar77j6lg0wypcc9uar5d2shskhj42g",
    ),
    (
        4,
        "addr1gx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer5pnz75xxcrzqf96k",
    ),
    (
        5,
        "addr128phkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtupnz75xxcrtw79hu",
    ),
    (
        6,
        "addr1vx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzers66hrl8",
    ),
    (
        7,
        "addr1w8phkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtcyjy7wx",
    ),
    (
        14,
        "stake1uyehkck0lajq8gr28t9uxnuvgcqrc6070x3k9r8048z8y5gh6ffgw",
    ),
    (
        15,
        "stake178phkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtcccycj5",
    ),
];

const TESTNET_VECTORS: [(u8, &str); 10] = [
    (
        0,
        "addr_test1qz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer3n0d3vllmyqwsx5wktcd8cc3sq835lu7drv2xwl2wywfgs68faae",
    ),
    (
        1,
        "addr_test1zrphkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gten0d3vllmyqwsx5wktcd8cc3sq835lu7drv2xwl2wywfgsxj90mg",
    ),
    (
        2,
        "addr_test1yz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerkr0vd4msrxnuwnccdxlhdjar77j6lg0wypcc9uar5d2shsf5r8qx",
    ),
    (
        3,
        "addr_test1xrphkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gt7r0vd4msrxnuwnccdxlhdjar77j6lg0wypcc9uar5d2shs4p04xh",
    ),
    (
        4,
        "addr_test1gz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer5pnz75xxcrdw5vky",
    ),
    (
        5,
        "addr_test12rphkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtupnz75xxcryqrvmw",
    ),
    (
        6,
        "addr_test1vz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerspjrlsz",
    ),
    (
        7,
        "addr_test1wrphkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtcl6szpr",
    ),
    (
        14,
        "stake_test1uqehkck0lajq8gr28t9uxnuvgcqrc6070x3k9r8048z8y5gssrtvn",
    ),
    (
        15,
        "stake_test17rphkx6acpnf78fuvxn0mkew3l0fd058hzquvz7w36x4gtcljw6kf",
    ),
];

/// The Strike staking script address (`solution/protocols/strike/strike-staking/.env.mainnet`).
const STRIKE_SCRIPT_ADDRESS: &str = "addr1z9yh4zcqs4gh78ysvh8nqp40fsnxg49nn3h6x25az9k8tms6409492020k6xml8uvwn34wrexagjh5fsk5xk96jyxk2qf3a7kj";

/// A Byron (Icarus-style) address without a network tag.
const BYRON_ICARUS: &str = "Ae2tdPwUPEZLs4HtbuNey7tK4hTKrwNwYtGqp7bDfCy2WdR3P6735W5Yfpe";
const BYRON_ICARUS_HEX: &str =
    "82d818582183581cf11939f42338d59e21baa08645ac1f0038d5ee969f99fe98f402fe79a0001ac9d64e5b";

/// A Byron (Daedalus-style) address without a network tag.
const BYRON_DAEDALUS: &str = "DdzFFzCqrht7PQiAhzrn6rNNoADJieTWBt8KeK9BZdUsGyX9ooYD9NpMCTGjQoUKcHN47g8JMXhvKogsGpQHtiQ65fZwiypjrC6d3a4Q";

/// The CIP-19 Byron example; its network tag names the legacy testnet.
const BYRON_CIP19: &str = "37btjrVyb4KDXBNC4haBVPCrro8AQPHwvCMp3RFhhSVWwfFmZ6wwzSK6JK1hY6wHNmtrpTf1kdbva8TCneM2YsiXT7mrzT21EacHnPpz5YyUdj64na";

/// A payment credential as `(type, hash)`.
type Payment = Option<(PaymentCredentialType, &'static str)>;
/// A stake credential as `(type, hash)`; pointers have no hash.
type Stake = Option<(StakeCredentialType, Option<&'static str>)>;

/// Expected `(kind, payment credential, stake credential)` of each CIP-19
/// address type.
fn expected_parts(address_type: u8) -> (AddressKind, Payment, Stake) {
    use PaymentCredentialType as P;
    use StakeCredentialType as S;
    let key = Some((P::KeyHash, PAYMENT_KEY_HASH));
    let script = Some((P::ScriptHash, SCRIPT_HASH));
    let stake_key = Some((S::KeyHash, Some(STAKE_KEY_HASH)));
    let stake_script = Some((S::ScriptHash, Some(SCRIPT_HASH)));
    let pointer = Some((S::Pointer, None));
    match address_type {
        0 => (AddressKind::ShelleyBase, key, stake_key),
        1 => (AddressKind::ShelleyBase, script, stake_key),
        2 => (AddressKind::ShelleyBase, key, stake_script),
        3 => (AddressKind::ShelleyBase, script, stake_script),
        4 => (AddressKind::ShelleyPointer, key, pointer),
        5 => (AddressKind::ShelleyPointer, script, pointer),
        6 => (AddressKind::ShelleyEnterprise, key, None),
        7 => (AddressKind::ShelleyEnterprise, script, None),
        14 => (AddressKind::Stake, None, stake_key),
        15 => (AddressKind::Stake, None, stake_script),
        other => panic!("no CIP-19 vector of type {other}"),
    }
}

fn assert_matches_vector(report: &AddressReport, address_type: u8, address: &str) {
    let (kind, payment, stake) = expected_parts(address_type);
    assert_eq!(report.kind, kind, "{address}");
    assert_eq!(
        report
            .payment_credential
            .as_ref()
            .map(|c| (c.kind, c.hash_hex.as_str())),
        payment,
        "{address}"
    );
    assert_eq!(
        report
            .stake_credential
            .as_ref()
            .map(|c| (c.kind, c.hash_hex.as_deref())),
        stake,
        "{address}"
    );
    let pointer = report
        .stake_credential
        .as_ref()
        .and_then(|c| c.pointer)
        .map(|p| (p.slot, p.tx_idx, p.cert_idx));
    let expected_pointer = matches!(address_type, 4 | 5).then_some(POINTER);
    assert_eq!(pointer, expected_pointer, "{address}");
    assert_eq!(report.input, address);
    assert_eq!(report.bech32.as_deref(), Some(address));
    assert!(
        report.hex.starts_with(&format!("{address_type:x}")),
        "{address}: header of {}",
        report.hex
    );
}

#[test]
fn decodes_every_cip19_mainnet_vector() {
    for (address_type, address) in MAINNET_VECTORS {
        let report = inspect(address).unwrap();
        assert_matches_vector(&report, address_type, address);
        assert_eq!(report.network, AddressNetwork::Mainnet, "{address}");
        assert_eq!(report.network_id, Some(1), "{address}");
        assert!(report.notes.is_empty(), "{address}: {:?}", report.notes);
    }
}

#[test]
fn decodes_every_cip19_testnet_vector() {
    for (address_type, address) in TESTNET_VECTORS {
        let report = inspect(address).unwrap();
        assert_matches_vector(&report, address_type, address);
        assert_eq!(
            report.network,
            AddressNetwork::PreprodOrPreview,
            "{address}"
        );
        assert_eq!(report.network_id, Some(0), "{address}");
        assert_eq!(report.notes, [TESTNET_NOTE], "{address}");
    }
}

#[test]
fn mainnet_base_address_has_the_documented_wire_shape() {
    let report = inspect(MAINNET_VECTORS[0].1).unwrap();
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        json!({
            "input": MAINNET_VECTORS[0].1,
            "kind": "shelley_base",
            "network": "mainnet",
            "network_id": 1,
            "payment_credential": { "type": "key_hash", "hash_hex": PAYMENT_KEY_HASH },
            "stake_credential": { "type": "key_hash", "hash_hex": STAKE_KEY_HASH, "pointer": null },
            "bech32": MAINNET_VECTORS[0].1,
            "hex": format!("01{PAYMENT_KEY_HASH}{STAKE_KEY_HASH}"),
            "notes": [],
        })
    );
}

#[test]
fn pointer_and_reward_addresses_have_the_documented_wire_shape() {
    let pointer = serde_json::to_value(inspect(MAINNET_VECTORS[4].1).unwrap()).unwrap();
    assert_eq!(pointer["kind"], "shelley_pointer");
    assert_eq!(
        pointer["stake_credential"],
        json!({
            "type": "pointer",
            "hash_hex": null,
            "pointer": { "slot": 2_498_243, "tx_idx": 27, "cert_idx": 3 },
        })
    );

    let reward = serde_json::to_value(inspect(MAINNET_VECTORS[8].1).unwrap()).unwrap();
    assert_eq!(reward["kind"], "stake");
    assert_eq!(reward["payment_credential"], json!(null));
    assert_eq!(reward["stake_credential"]["hash_hex"], STAKE_KEY_HASH);
}

#[test]
fn reports_the_strike_script_address_as_a_script_payment_credential() {
    let report = inspect(STRIKE_SCRIPT_ADDRESS).unwrap();
    assert_eq!(report.kind, AddressKind::ShelleyBase);
    assert_eq!(report.network, AddressNetwork::Mainnet);
    let payment = report.payment_credential.unwrap();
    assert_eq!(payment.kind, PaymentCredentialType::ScriptHash);
    assert_eq!(
        payment.hash_hex,
        "497a8b0085517f1c9065cf3006af4c266454b39c6fa32a9d116c75ee"
    );
    let stake = report.stake_credential.unwrap();
    assert_eq!(stake.kind, StakeCredentialType::KeyHash);
    assert_eq!(
        stake.hash_hex.as_deref(),
        Some("1aabcb52a9ea7db46dfcfc63a71ab87937512bd130b50d62ea443594")
    );
}

#[test]
fn byron_addresses_report_no_credentials() {
    for address in [BYRON_ICARUS, BYRON_DAEDALUS] {
        let report = inspect(address).unwrap();
        assert_eq!(report.kind, AddressKind::Byron, "{address}");
        assert_eq!(report.network, AddressNetwork::Unknown, "{address}");
        assert_eq!(report.network_id, None, "{address}");
        assert_eq!(report.payment_credential, None, "{address}");
        assert_eq!(report.stake_credential, None, "{address}");
        assert_eq!(report.bech32, None, "{address}");
        assert_eq!(report.notes.len(), 2, "{address}: {:?}", report.notes);
        assert!(
            report.notes[1].contains("no network tag"),
            "{:?}",
            report.notes
        );
    }
    assert_eq!(inspect(BYRON_ICARUS).unwrap().hex, BYRON_ICARUS_HEX);
}

#[test]
fn byron_network_comes_from_the_network_tag() {
    let legacy = inspect(BYRON_CIP19).unwrap();
    assert_eq!(legacy.network, AddressNetwork::Unknown);
    assert!(
        legacy.notes[1].contains("protocol magic 1097911063 (the retired legacy testnet)"),
        "{:?}",
        legacy.notes
    );

    for (magic, network, name) in [
        (764_824_073, AddressNetwork::Mainnet, "mainnet"),
        (1, AddressNetwork::PreprodOrPreview, "preprod"),
        (2, AddressNetwork::PreprodOrPreview, "preview"),
    ] {
        let report = inspect(&byron_with_network_tag(magic)).unwrap();
        assert_eq!(report.kind, AddressKind::Byron);
        assert_eq!(report.network, network, "magic {magic}");
        assert_eq!(report.network_id, None);
        assert!(
            report.notes[1].contains(&format!("protocol magic {magic} ({name})")),
            "{:?}",
            report.notes
        );
    }

    let unknown = inspect(&byron_with_network_tag(42)).unwrap();
    assert_eq!(unknown.network, AddressNetwork::Unknown);
    assert!(unknown.notes[1].contains("not a known public network"));
}

/// A base58 Byron public-key address whose network tag carries `magic`.
fn byron_with_network_tag(magic: u32) -> String {
    let tag = minicbor::to_vec(magic).expect("encode protocol magic");
    let payload = AddressPayload::new(
        AddrType::PubKey,
        SpendingData::PubKey(ByteVec::from(vec![7u8; 64])),
        vec![AddrAttrProperty::NetworkTag(ByteVec::from(tag))].into(),
    );
    ByronAddress::from_decoded(payload).to_base58()
}

#[test]
fn accepts_hex_and_uppercase_bech32() {
    let (_, address) = MAINNET_VECTORS[0];
    let expected = inspect(address).unwrap();

    let from_hex = inspect(&expected.hex).unwrap();
    assert_eq!(from_hex.input, expected.hex);
    assert_eq!(
        AddressReport {
            input: address.into(),
            ..from_hex
        },
        expected
    );

    let from_upper_hex = inspect(&expected.hex.to_uppercase()).unwrap();
    assert!(
        from_upper_hex.notes.is_empty(),
        "{:?}",
        from_upper_hex.notes
    );

    let upper = address.to_uppercase();
    let from_upper = inspect(&upper).unwrap();
    assert_eq!(
        AddressReport {
            input: address.into(),
            ..from_upper
        },
        expected
    );

    let byron = inspect(BYRON_ICARUS_HEX).unwrap();
    assert_eq!(byron.kind, AddressKind::Byron);
    assert_eq!(
        AddressReport {
            input: BYRON_ICARUS.into(),
            ..byron
        },
        inspect(BYRON_ICARUS).unwrap()
    );
}

#[test]
fn ignores_surrounding_whitespace() {
    let (_, address) = TESTNET_VECTORS[6];
    let report = inspect(&format!("  {address}\n")).unwrap();
    assert_eq!(report.input, address);
    assert_eq!(report.kind, AddressKind::ShelleyEnterprise);
}

#[test]
fn notes_input_that_is_not_canonically_encoded() {
    // A mainnet base address with trailing bytes, as minted on chain.
    let hex = "015bad085057ac10ecc7060f7ac41edd6f63068d8963ef7d86ca58669e5ecf2d283418a60be5a848a2380eb721000da1e0bbf39733134beca4cb57afb0b35fc89c63061c9914e055001a518c7516";
    let report = inspect(hex).unwrap();
    assert_eq!(report.kind, AddressKind::ShelleyBase);
    assert_ne!(report.hex, hex);
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.contains("does not re-encode")),
        "{:?}",
        report.notes
    );
}

#[test]
fn reports_network_ids_other_than_mainnet_and_testnet_as_unknown() {
    // Type-6 header on network id 3.
    let report = inspect(&format!("63{PAYMENT_KEY_HASH}")).unwrap();
    assert_eq!(report.kind, AddressKind::ShelleyEnterprise);
    assert_eq!(report.network, AddressNetwork::Unknown);
    assert_eq!(report.network_id, Some(3));
    assert_eq!(report.bech32, None);
    assert!(
        report.notes[0].contains("Network id 3"),
        "{:?}",
        report.notes
    );
}

/// Asserts `input` is rejected as `invalid_arguments` naming `address`, and that
/// neither the message nor the details repeat more than 16 of its characters.
fn assert_rejected(input: &str) -> FluentError {
    let err = inspect(input).expect_err(input);
    assert_eq!(err.code(), ErrorCode::InvalidArguments, "{input}");
    assert_eq!(
        err.details(),
        Some(json!({ "arguments": ["address"] })),
        "{input}"
    );
    let message = err.message();
    let details = err.details().expect("details").to_string();
    let chars: Vec<char> = input.chars().collect();
    for window in chars.windows(17) {
        let window: String = window.iter().collect();
        assert!(!message.contains(&window), "{message} echoes {input}");
        assert!(!details.contains(&window), "{details} echoes {input}");
    }
    err
}

#[test]
fn rejects_malformed_input_as_invalid_arguments() {
    let (_, address) = MAINNET_VECTORS[0];
    let mut bad_checksum = address.to_owned();
    bad_checksum.pop();
    bad_checksum.push('q');

    for (input, reason) in [
        ("", "empty"),
        ("   ", "empty"),
        ("not an address at all", "not valid bech32, hex or base58"),
        (&bad_checksum, "not valid bech32"),
        (&address[..40], "not valid bech32"),
        ("0142", "not valid address bytes"),
        ("abc", "not valid address bytes"),
        ("ff00", "not valid address bytes"),
        (&BYRON_ICARUS[..30], "not valid bech32, hex or base58"),
        // Mainnet base-address bytes under the testnet prefix.
        (
            "addr_test1qx2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer3n0d3vllmyqwsx5wktcd8cc3sq835lu7drv2xwl2wywfgst3q60y",
            "requires `addr`",
        ),
        // Reward-address bytes under a payment-address prefix.
        (
            "addr1uyehkck0lajq8gr28t9uxnuvgcqrc6070x3k9r8048z8y5g3zl0gu",
            "requires `stake`",
        ),
        // A Byron address whose checksum was altered.
        (
            "82d818582183581cf11939f42338d59e21baa08645ac1f0038d5ee969f99fe98f402fe79a0001ac9d64e5c",
            "checksum does not match",
        ),
    ] {
        let err = assert_rejected(input);
        assert!(
            err.message().starts_with("invalid arguments: address "),
            "{input:?}: {}",
            err.message()
        );
        assert!(
            err.message().contains(reason),
            "{input:?}: {}",
            err.message()
        );
    }
}

#[test]
fn report_round_trips_through_json_and_derives_a_schema() {
    for (_, address) in MAINNET_VECTORS.iter().chain(&TESTNET_VECTORS) {
        let report = inspect(address).unwrap();
        let text = serde_json::to_string(&report).unwrap();
        let parsed: AddressReport = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, report);
    }

    let schema = serde_json::to_value(schemars::schema_for!(AddressReport)).unwrap();
    let mut properties: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    properties.sort_unstable();
    assert_eq!(
        properties,
        [
            "bech32",
            "hex",
            "input",
            "kind",
            "network",
            "network_id",
            "notes",
            "payment_credential",
            "stake_credential",
        ]
    );
}
