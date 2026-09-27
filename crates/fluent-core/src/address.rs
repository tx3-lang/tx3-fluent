//! Offline inspection of Cardano addresses.
//!
//! [`inspect`] decodes one address — a Shelley payment or stake address in
//! bech32 or hex, or a Byron address in base58 or hex — into an
//! [`AddressReport`]: its kind, network and credentials, read from the address
//! alone. Nothing is looked up on chain, so the report cannot say whether the
//! address has been used or what it holds.

use pallas_addresses::byron::{AddrAttrProperty, ByronAddress};
use pallas_addresses::{
    Address, Network, Pointer, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart,
    StakeAddress, StakePayload,
};
use pallas_codec::minicbor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::FluentError;

/// The note on every report for a testnet (network id 0) address.
pub const TESTNET_NOTE: &str =
    "Testnet addresses do not distinguish preprod from preview; confirm the network with the user.";

const BYRON_CREDENTIALS_NOTE: &str = "Byron addresses carry no payment or stake credential \
hash, so they cannot supply key-hash arguments.";

const BYRON_UNTAGGED_NOTE: &str = "This Byron address has no network tag, which is how \
mainnet Byron addresses are encoded; confirm the network with the user.";

const NON_CANONICAL_NOTE: &str = "The input does not re-encode to itself (for example, it has \
trailing bytes or a non-canonical pointer); `bech32` and `hex` show the canonical encoding of \
the decoded parts.";

/// The argument name [`inspect`] errors report.
const ARGUMENT: &str = "address";

const MAINNET_PROTOCOL_MAGIC: u32 = 764_824_073;
const PREPROD_PROTOCOL_MAGIC: u32 = 1;
const PREVIEW_PROTOCOL_MAGIC: u32 = 2;
const LEGACY_TESTNET_PROTOCOL_MAGIC: u32 = 1_097_911_063;

/// The CRC Byron addresses carry over their payload.
const BYRON_CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// Facts decoded from one Cardano address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddressReport {
    /// The inspected address, without surrounding whitespace.
    pub input: String,
    /// What kind of address it is.
    pub kind: AddressKind,
    /// The network the address belongs to, as far as the address can tell.
    pub network: AddressNetwork,
    /// The network id in the address header; `null` for Byron addresses, which
    /// have none.
    pub network_id: Option<u8>,
    /// The credential that controls spending; `null` for stake and Byron
    /// addresses.
    pub payment_credential: Option<PaymentCredential>,
    /// The credential that controls delegation; `null` for enterprise and Byron
    /// addresses.
    pub stake_credential: Option<StakeCredential>,
    /// The canonical bech32 encoding; `null` for Byron addresses and for
    /// network ids that have no bech32 prefix.
    pub bech32: Option<String>,
    /// The canonical address bytes, hex encoded.
    pub hex: String,
    /// Caveats for whoever acts on the report, such as how far `network` can
    /// be trusted.
    pub notes: Vec<String>,
}

/// The kind of a Cardano address (CIP-19 address types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    /// A payment address with a stake credential hash (types 0–3).
    ShelleyBase,
    /// A payment address without a stake credential (types 6–7).
    ShelleyEnterprise,
    /// A payment address whose stake credential is a chain pointer (types 4–5).
    ShelleyPointer,
    /// A reward (stake) address (types 14–15).
    Stake,
    /// A Byron-era address (type 8).
    Byron,
}

/// The network an address belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AddressNetwork {
    /// Cardano mainnet.
    Mainnet,
    /// A public testnet; the address cannot tell preprod from preview.
    PreprodOrPreview,
    /// The address does not identify its network.
    Unknown,
}

/// The credential in the payment part of a Shelley address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaymentCredential {
    /// Whether a key or a script controls spending.
    #[serde(rename = "type")]
    pub kind: PaymentCredentialType,
    /// The 28-byte credential hash, hex encoded.
    pub hash_hex: String,
}

/// What a payment credential hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaymentCredentialType {
    /// The hash of a payment verification key.
    KeyHash,
    /// The hash of a script.
    ScriptHash,
}

/// The stake credential of a Shelley base, pointer or stake address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StakeCredential {
    /// Whether a key hash, a script hash or a chain pointer identifies the
    /// stake credential.
    #[serde(rename = "type")]
    pub kind: StakeCredentialType,
    /// The 28-byte credential hash, hex encoded; `null` for a pointer.
    pub hash_hex: Option<String>,
    /// The chain position of the stake registration; `null` unless `type` is
    /// `pointer`.
    pub pointer: Option<StakePointer>,
}

/// How a stake credential is identified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StakeCredentialType {
    /// The hash of a stake verification key.
    KeyHash,
    /// The hash of a script.
    ScriptHash,
    /// A pointer to the certificate that registered the stake credential.
    Pointer,
}

/// The chain position of a stake registration certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StakePointer {
    /// The absolute slot of the block holding the certificate.
    pub slot: u64,
    /// The transaction's index within that block.
    pub tx_idx: u64,
    /// The certificate's index within that transaction.
    pub cert_idx: u64,
}

/// Decodes one Cardano address into an [`AddressReport`].
///
/// Accepts bech32 and hex for Shelley and stake addresses, and base58 and hex
/// for Byron addresses. Surrounding whitespace is ignored.
///
/// # Errors
///
/// [`FluentError::InvalidArguments`] naming the `address` argument when the
/// input is not a well-formed address. The reason describes the parse failure
/// without quoting the input.
pub fn inspect(address: &str) -> Result<AddressReport, FluentError> {
    let input = address.trim();
    let (decoded, encoding) = decode(input).map_err(invalid)?;
    let mut report = match &decoded {
        Address::Shelley(shelley) => shelley_report(input, shelley),
        Address::Stake(stake) => stake_report(input, stake),
        Address::Byron(byron) => byron_report(input, byron).map_err(invalid)?,
    };

    let canonical = match (&decoded, encoding) {
        (_, Encoding::Bech32) => report
            .bech32
            .as_deref()
            .map(|b| b.eq_ignore_ascii_case(input)),
        (_, Encoding::Hex) => Some(report.hex.eq_ignore_ascii_case(input)),
        (Address::Byron(byron), Encoding::Base58) => Some(byron.to_base58() == input),
        (_, Encoding::Base58) => None,
    };
    if canonical == Some(false) {
        report.notes.push(NON_CANONICAL_NOTE.to_owned());
    }
    Ok(report)
}

/// The text form an address was given in.
#[derive(Debug, Clone, Copy)]
enum Encoding {
    Bech32,
    Base58,
    Hex,
}

fn invalid(reason: String) -> FluentError {
    FluentError::InvalidArguments {
        reason,
        arguments: vec![ARGUMENT.to_owned()],
    }
}

/// Parses `input` in whichever encoding it uses. Errors are reasons for
/// [`FluentError::InvalidArguments`] and never quote the input.
fn decode(input: &str) -> Result<(Address, Encoding), String> {
    if input.is_empty() {
        return Err("address is empty".to_owned());
    }

    // Bech32 addresses contain letters outside the hex alphabet and base58
    // excludes `0`, so an all-hex input is only plausible as hex.
    if input.bytes().all(|b| b.is_ascii_hexdigit()) {
        let address = Address::from_hex(input).map_err(|err| {
            format!(
                "address is hex but not valid address bytes: {}",
                describe(&err)
            )
        })?;
        return Ok((address, Encoding::Hex));
    }

    let bech32_err = match Address::from_bech32(input) {
        Ok(address) => {
            check_bech32_prefix(&address, input)?;
            return Ok((address, Encoding::Bech32));
        }
        Err(err) => err,
    };
    let base58_err = match ByronAddress::from_base58(input) {
        Ok(byron) => return Ok((Address::Byron(byron), Encoding::Base58)),
        Err(err) => err,
    };

    // Report the failure of the encoding the input most resembles.
    if looks_like_bech32(input) {
        Err(format!(
            "address is not valid bech32: {}",
            describe(&bech32_err)
        ))
    } else {
        Err(format!(
            "address is not valid bech32, hex or base58: {}",
            describe(&base58_err)
        ))
    }
}

/// True when `input` has a bech32 human-readable prefix: letters and `_` before
/// the last `1`.
fn looks_like_bech32(input: &str) -> bool {
    input.rfind('1').is_some_and(|separator| {
        separator > 0
            && input[..separator]
                .bytes()
                .all(|b| b.is_ascii_alphabetic() || b == b'_')
    })
}

/// Rejects bech32 input whose prefix disagrees with the network and kind in the
/// address header, which pallas does not check.
fn check_bech32_prefix(address: &Address, input: &str) -> Result<(), String> {
    if let Address::Byron(_) = address {
        return Err("address is a Byron address in bech32; Byron addresses are base58".to_owned());
    }
    let prefix = input
        .rfind('1')
        .map(|separator| input[..separator].to_ascii_lowercase());
    match address.hrp() {
        Ok(expected) if prefix.as_deref() != Some(expected) => Err(format!(
            "address bech32 prefix does not match its header, which requires `{expected}`"
        )),
        _ => Ok(()),
    }
}

/// An error and its sources, joined with `: `.
fn describe(err: &pallas_addresses::Error) -> String {
    // pallas does not expose the bech32 error as a source; start from it.
    let mut current: Option<&dyn std::error::Error> = match err {
        pallas_addresses::Error::BadBech32(inner) => Some(inner),
        pallas_addresses::Error::BadBase58(_) => return "not valid base58".to_owned(),
        other => Some(other),
    };
    let mut parts = Vec::new();
    while let Some(err) = current {
        parts.push(err.to_string());
        current = err.source();
    }
    parts.join(": ")
}

fn shelley_report(input: &str, address: &ShelleyAddress) -> AddressReport {
    let payment_credential = match address.payment() {
        ShelleyPaymentPart::Key(hash) => PaymentCredential {
            kind: PaymentCredentialType::KeyHash,
            hash_hex: hex::encode(hash),
        },
        ShelleyPaymentPart::Script(hash) => PaymentCredential {
            kind: PaymentCredentialType::ScriptHash,
            hash_hex: hex::encode(hash),
        },
    };
    let (kind, stake_credential) = match address.delegation() {
        ShelleyDelegationPart::Key(hash) => (
            AddressKind::ShelleyBase,
            Some(StakeCredential::hash(StakeCredentialType::KeyHash, hash)),
        ),
        ShelleyDelegationPart::Script(hash) => (
            AddressKind::ShelleyBase,
            Some(StakeCredential::hash(StakeCredentialType::ScriptHash, hash)),
        ),
        ShelleyDelegationPart::Pointer(pointer) => (
            AddressKind::ShelleyPointer,
            Some(StakeCredential::pointer(pointer)),
        ),
        ShelleyDelegationPart::Null => (AddressKind::ShelleyEnterprise, None),
    };
    let (network, notes) = header_network(address.network());

    AddressReport {
        input: input.to_owned(),
        kind,
        network,
        network_id: Some(address.network().value()),
        payment_credential: Some(payment_credential),
        stake_credential,
        bech32: address.to_bech32().ok(),
        hex: address.to_hex(),
        notes,
    }
}

fn stake_report(input: &str, address: &StakeAddress) -> AddressReport {
    let stake_credential = match address.payload() {
        StakePayload::Stake(hash) => StakeCredential::hash(StakeCredentialType::KeyHash, hash),
        StakePayload::Script(hash) => StakeCredential::hash(StakeCredentialType::ScriptHash, hash),
    };
    let (network, notes) = header_network(address.network());

    AddressReport {
        input: input.to_owned(),
        kind: AddressKind::Stake,
        network,
        network_id: Some(address.network().value()),
        payment_credential: None,
        stake_credential: Some(stake_credential),
        bech32: address.to_bech32().ok(),
        hex: address.to_hex(),
        notes,
    }
}

fn byron_report(input: &str, address: &ByronAddress) -> Result<AddressReport, String> {
    // pallas decodes Byron addresses without verifying their checksum.
    if BYRON_CRC.checksum(&address.payload.0) != address.crc {
        return Err(
            "address is a Byron address whose checksum does not match; it is mistyped or corrupted"
                .to_owned(),
        );
    }
    let payload = address.decode().map_err(|err| {
        format!(
            "address is a Byron address with an invalid payload: {}",
            describe(&err)
        )
    })?;

    let mut notes = vec![BYRON_CREDENTIALS_NOTE.to_owned()];
    let tag = payload
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            AddrAttrProperty::NetworkTag(tag) => Some(tag),
            _ => None,
        });
    let network = match tag.map(|tag| minicbor::decode::<u32>(tag)) {
        None => {
            notes.push(BYRON_UNTAGGED_NOTE.to_owned());
            AddressNetwork::Unknown
        }
        Some(Ok(magic)) => {
            let prefix = format!("The Byron network tag carries protocol magic {magic}");
            let (network, note) = match magic {
                MAINNET_PROTOCOL_MAGIC => (AddressNetwork::Mainnet, format!("{prefix} (mainnet).")),
                PREPROD_PROTOCOL_MAGIC => (
                    AddressNetwork::PreprodOrPreview,
                    format!("{prefix} (preprod)."),
                ),
                PREVIEW_PROTOCOL_MAGIC => (
                    AddressNetwork::PreprodOrPreview,
                    format!("{prefix} (preview)."),
                ),
                LEGACY_TESTNET_PROTOCOL_MAGIC => (
                    AddressNetwork::Unknown,
                    format!(
                        "{prefix} (the retired legacy testnet); confirm the network with the user."
                    ),
                ),
                _ => (
                    AddressNetwork::Unknown,
                    format!(
                        "{prefix}, which is not a known public network; confirm the network \
                         with the user."
                    ),
                ),
            };
            notes.push(note);
            network
        }
        Some(Err(_)) => {
            notes.push(
                "The Byron network tag could not be decoded; confirm the network with the user."
                    .to_owned(),
            );
            AddressNetwork::Unknown
        }
    };

    Ok(AddressReport {
        input: input.to_owned(),
        kind: AddressKind::Byron,
        network,
        network_id: None,
        payment_credential: None,
        stake_credential: None,
        bech32: None,
        hex: address.to_hex(),
        notes,
    })
}

/// The network a Shelley or stake header names, with any caveat.
fn header_network(network: Network) -> (AddressNetwork, Vec<String>) {
    match network {
        Network::Mainnet => (AddressNetwork::Mainnet, Vec::new()),
        Network::Testnet => (
            AddressNetwork::PreprodOrPreview,
            vec![TESTNET_NOTE.to_owned()],
        ),
        Network::Other(id) => (
            AddressNetwork::Unknown,
            vec![format!(
                "Network id {id} is neither mainnet (1) nor testnet (0); confirm the network \
                 with the user."
            )],
        ),
    }
}

impl StakeCredential {
    fn hash(kind: StakeCredentialType, hash: &pallas_primitives::Hash<28>) -> Self {
        StakeCredential {
            kind,
            hash_hex: Some(hex::encode(hash)),
            pointer: None,
        }
    }

    fn pointer(pointer: &Pointer) -> Self {
        StakeCredential {
            kind: StakeCredentialType::Pointer,
            hash_hex: None,
            pointer: Some(StakePointer {
                slot: pointer.slot(),
                tx_idx: pointer.tx_idx(),
                cert_idx: pointer.cert_idx(),
            }),
        }
    }
}
