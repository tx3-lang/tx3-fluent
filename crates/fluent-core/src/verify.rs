//! An independent check of a transaction's CBOR against what a person
//! expects it to do.
//!
//! [`verify`] decodes the CBOR with [`summary::try_decode`], which reads the
//! CBOR only, and checks the [`Summary`] against [`Expectations`]:
//!
//! - each [`OutputExpectation`] must match its own output: the same address,
//!   exactly the expected lovelace and exactly the expected native assets,
//!   no more and no fewer;
//! - every output address must belong to the expected network (mainnet, or a
//!   testnet: addresses cannot tell preprod from preview);
//! - each expected signer must be among the required signers.
//!
//! The [`Verdict`] is [`Verdict::Match`] only when every check passes. Like
//! the summary, the check is a pure function: no I/O, no clock, no
//! configuration.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::address::{self, AddressNetwork};
use crate::envelope::{AssetAmount, OutputSummary, TransactionSummary};
use crate::error::FluentError;
use crate::registration::Network;
use crate::summary::{self, Summary};

/// One output the transaction must contain, written
/// `<address>=<lovelace>[+<policy_id>.<asset_name_hex>=<amount>]...`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputExpectation {
    /// The address, as the summary spells it: bech32, base58 for Byron.
    pub address: String,
    /// Exactly the lovelace the output carries.
    pub lovelace: u64,
    /// Exactly the native assets the output carries, ordered by policy id,
    /// then asset name, as the summary orders them.
    pub assets: Vec<AssetAmount>,
}

impl FromStr for OutputExpectation {
    type Err = FluentError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = |reason: String| invalid("expect-output", reason);
        let (address, value) = text.trim().split_once('=').ok_or_else(|| {
            invalid(
                "an expected output is `<address>=<lovelace>[+<policy>.<asset>=<amount>]`".into(),
            )
        })?;
        let report = address::inspect(address)
            .map_err(|_| invalid("an expected output's address cannot be decoded".into()))?;
        let address = report.bech32.unwrap_or(report.input);

        let mut parts = value.split('+');
        let lovelace = parts
            .next()
            .unwrap_or_default()
            .parse::<u64>()
            .map_err(|_| invalid("an expected output's lovelace is not a whole number".into()))?;
        let mut assets = parts
            .map(|asset| parse_asset(asset).map_err(invalid))
            .collect::<Result<Vec<_>, _>>()?;
        assets.sort_by(|a, b| {
            (&a.policy_id, &a.asset_name_hex).cmp(&(&b.policy_id, &b.asset_name_hex))
        });
        if assets.windows(2).any(|w| {
            (&w[0].policy_id, &w[0].asset_name_hex) == (&w[1].policy_id, &w[1].asset_name_hex)
        }) {
            return Err(invalid(
                "an expected output names the same asset twice".into(),
            ));
        }
        Ok(OutputExpectation {
            address,
            lovelace,
            assets,
        })
    }
}

impl fmt::Display for OutputExpectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.address, self.lovelace)?;
        for asset in &self.assets {
            write!(
                f,
                "+{}.{}={}",
                asset.policy_id, asset.asset_name_hex, asset.amount
            )?;
        }
        Ok(())
    }
}

/// `<policy_id>.<asset_name_hex>=<amount>`; the asset name may be empty.
fn parse_asset(text: &str) -> Result<AssetAmount, String> {
    let shape = "an expected asset is `<policy_id>.<asset_name_hex>=<amount>`";
    let (unit, amount) = text.split_once('=').ok_or_else(|| shape.to_string())?;
    let (policy_id, asset_name_hex) = unit.split_once('.').ok_or_else(|| shape.to_string())?;
    let is_hex = |s: &str| s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if policy_id.len() != 56 || !is_hex(policy_id) {
        return Err("an expected asset's policy id is not 56 hex digits".into());
    }
    if asset_name_hex.len() > 64 || !is_hex(asset_name_hex) {
        return Err("an expected asset's name is not at most 32 hex-encoded bytes".into());
    }
    let amount = amount
        .parse::<u64>()
        .map_err(|_| "an expected asset's amount is not a whole number".to_string())?;
    Ok(AssetAmount {
        policy_id: policy_id.to_ascii_lowercase(),
        asset_name_hex: asset_name_hex.to_ascii_lowercase(),
        amount,
    })
}

/// What the transaction is expected to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectations {
    /// Outputs the transaction must contain; at least one.
    pub outputs: Vec<OutputExpectation>,
    /// The network every output address must belong to.
    pub network: Network,
    /// Key hashes (28 bytes, hex) that must be required signers.
    pub signers: Vec<String>,
}

/// The outcome of [`verify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every check passed.
    Match,
    /// At least one check failed.
    Mismatch,
}

impl Verdict {
    /// The verdict's serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Match => "match",
            Verdict::Mismatch => "mismatch",
        }
    }
}

/// What a [`Check`] compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    /// An expected output.
    Output,
    /// The output addresses' network.
    Network,
    /// An expected required signer.
    Signer,
}

/// One comparison between the decoded transaction and an expectation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// What was compared.
    pub check: CheckKind,
    /// The expectation, as it was written.
    pub expected: String,
    /// Whether the transaction meets it.
    pub passed: bool,
    /// What the transaction holds instead, or which output matched.
    pub detail: String,
}

/// The decoded transaction, every check and the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// The Blake2b-256 of the transaction body bytes, hex encoded.
    pub tx_hash: String,
    /// The decoded view of the transaction.
    pub summary: TransactionSummary,
    /// Every comparison, outputs first, then the network, then signers.
    pub checks: Vec<Check>,
    /// [`Verdict::Match`] when every check passed.
    pub verdict: Verdict,
}

/// Decodes `tx_hex` and checks it against `expected`; see the [module
/// documentation](self).
///
/// Fails as [`FluentError::InvalidArguments`] when `tx_hex` is not one
/// complete Conway-era transaction or `expected` names no output or a signer
/// that is not a key hash. A transaction that does not meet the expectations
/// is not an error: it is a [`Verification`] with [`Verdict::Mismatch`].
pub fn verify(tx_hex: &str, expected: &Expectations) -> Result<Verification, FluentError> {
    if expected.outputs.is_empty() {
        return Err(invalid(
            "expect-output",
            "at least one expected output is required".into(),
        ));
    }
    let signers = expected
        .signers
        .iter()
        .map(|signer| {
            let signer = signer.trim().to_ascii_lowercase();
            if signer.len() == 56 && signer.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(signer)
            } else {
                Err(invalid(
                    "expect-signer",
                    "an expected signer is not a 28-byte key hash in hex".into(),
                ))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Summary {
        tx_hash,
        transaction,
    } = summary::try_decode(tx_hex.trim())
        .map_err(|err| invalid("cbor", format!("the transaction cannot be decoded: {err}")))?;

    let mut checks = Vec::new();
    let mut used = vec![false; transaction.outputs.len()];
    for expectation in &expected.outputs {
        checks.push(check_output(expectation, &transaction.outputs, &mut used));
    }
    checks.push(check_network(expected.network, &transaction.outputs));
    for signer in signers {
        let passed = transaction.required_signers.contains(&signer);
        checks.push(Check {
            check: CheckKind::Signer,
            detail: if passed {
                "a required signer".into()
            } else {
                format!(
                    "not among the {} required signers",
                    transaction.required_signers.len()
                )
            },
            expected: signer,
            passed,
        });
    }

    let verdict = if checks.iter().all(|c| c.passed) {
        Verdict::Match
    } else {
        Verdict::Mismatch
    };
    Ok(Verification {
        tx_hash,
        summary: transaction,
        checks,
        verdict,
    })
}

/// Matches `expected` against the first output not matched yet that pays
/// the same address the same value.
fn check_output(
    expected: &OutputExpectation,
    outputs: &[OutputSummary],
    used: &mut [bool],
) -> Check {
    let same_address = |o: &OutputSummary| o.address.eq_ignore_ascii_case(&expected.address);
    let found = outputs.iter().enumerate().position(|(index, output)| {
        !used[index]
            && same_address(output)
            && output.lovelace == expected.lovelace
            && output.assets == expected.assets
    });
    let (passed, detail) = match found {
        Some(index) => {
            used[index] = true;
            (true, format!("output {index}"))
        }
        None => {
            let paying: Vec<String> = outputs
                .iter()
                .enumerate()
                .filter(|(_, output)| same_address(output))
                .map(|(index, output)| {
                    let assets = output
                        .assets
                        .iter()
                        .map(|a| format!("+{}.{}={}", a.policy_id, a.asset_name_hex, a.amount))
                        .collect::<String>();
                    format!("output {index} carries {}{assets}", output.lovelace)
                })
                .collect();
            let detail = if paying.is_empty() {
                "no output pays this address".to_string()
            } else {
                format!(
                    "no unmatched output carries exactly this value; {}",
                    paying.join("; ")
                )
            };
            (false, detail)
        }
    };
    Check {
        check: CheckKind::Output,
        expected: expected.to_string(),
        passed,
        detail,
    }
}

/// Every output address must belong to `network`.
fn check_network(network: Network, outputs: &[OutputSummary]) -> Check {
    let wanted = match network {
        Network::Mainnet => AddressNetwork::Mainnet,
        Network::Preprod | Network::Preview => AddressNetwork::PreprodOrPreview,
    };
    let wrong: Vec<String> = outputs
        .iter()
        .enumerate()
        .filter_map(|(index, output)| {
            let found = address::inspect(&output.address)
                .map(|report| report.network)
                .unwrap_or(AddressNetwork::Unknown);
            (found != wanted).then(|| format!("output {index} is on {}", describe(found)))
        })
        .collect();
    let passed = wrong.is_empty();
    Check {
        check: CheckKind::Network,
        expected: network.as_str().to_string(),
        passed,
        detail: if passed {
            format!("every output address is on {}", describe(wanted))
        } else {
            wrong.join("; ")
        },
    }
}

fn describe(network: AddressNetwork) -> &'static str {
    match network {
        AddressNetwork::Mainnet => "mainnet",
        AddressNetwork::PreprodOrPreview => "a testnet (preprod or preview)",
        AddressNetwork::Unknown => "an unknown network",
    }
}

fn invalid(argument: &str, reason: String) -> FluentError {
    FluentError::InvalidArguments {
        reason,
        arguments: vec![argument.to_string()],
        violations: Vec::new(),
    }
}
