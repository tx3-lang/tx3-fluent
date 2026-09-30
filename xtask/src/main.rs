//! `cargo xtask`: Tx3 Fluent development and evidence tooling. See the
//! [library](xtask) for what each command does.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use fluent_core::FluentError;
use fluent_core::registration::Network;
use serde_json::{Value, json};
use xtask::transcript;
use xtask::verify::{self, Expectations, OutputExpectation, Verdict};

/// Tx3 Fluent development and evidence tooling; never shipped.
#[derive(Debug, Parser)]
#[command(name = "cargo xtask")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Decode a transaction's CBOR on its own, without the arguments, the
    /// resolver or the runtime's summary decoder, and check it against the
    /// outputs, network and signers you expect. Prints the summary, every
    /// check and the verdict as JSON; exits 0 only on `match`.
    Verify {
        /// The transaction CBOR in hex, or `-` to read it from stdin.
        #[arg(long, value_name = "HEX")]
        cbor: String,
        /// An output the transaction must contain, exactly:
        /// `<address>=<lovelace>[+<policy_id>.<asset_name_hex>=<amount>]...`.
        /// Repeat for several outputs.
        #[arg(long, value_name = "OUTPUT", required = true)]
        expect_output: Vec<String>,
        /// The network every output address must belong to: `mainnet`,
        /// `preprod` or `preview` (addresses cannot tell the testnets apart).
        #[arg(long, value_name = "NETWORK")]
        expect_network: String,
        /// A key hash (28 bytes, hex) that must be a required signer. Repeat
        /// for several.
        #[arg(long, value_name = "HASH")]
        expect_signer: Vec<String>,
    },
    /// Render a Markdown transcript of every `tools/list` and `tools/call` a
    /// server answered from its JSON log lines, such as saved `kubectl logs`
    /// output. Other lines are skipped.
    Transcript {
        /// A log file to read; repeat for several (entries are ordered by
        /// time). Reads stdin when none is given.
        #[arg(long, value_name = "FILE")]
        logs: Vec<PathBuf>,
        /// Keep only this principal's entries, by its `sub_hash` (the first
        /// 12 hex digits of the SHA-256 of its `sub`). Repeat for several.
        #[arg(long, value_name = "HASH")]
        sub_hash: Vec<String>,
        /// Write the transcript here instead of stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Verify {
            cbor,
            expect_output,
            expect_network,
            expect_signer,
        } => verify_cbor(cbor, &expect_output, &expect_network, expect_signer),
        Command::Transcript {
            logs,
            sub_hash,
            out,
        } => render_transcript(&logs, &sub_hash, out),
    }
}

/// `cargo xtask verify`: prints the verification, or the error as
/// `{"error": {code, message, details}}`, as JSON; exits 0 only on a match.
fn verify_cbor(
    cbor: String,
    outputs: &[String],
    network: &str,
    signers: Vec<String>,
) -> anyhow::Result<ExitCode> {
    let cbor = if cbor == "-" {
        std::io::read_to_string(std::io::stdin()).context("reading the CBOR from stdin")?
    } else {
        cbor
    };
    let result = expectations(outputs, network, signers)
        .and_then(|expected| verify::verify(&cbor, &expected));
    let (printed, code) = match result {
        Ok(verification) => {
            eprintln!("verdict: {}", verification.verdict.as_str());
            let code = match verification.verdict {
                Verdict::Match => ExitCode::SUCCESS,
                Verdict::Mismatch => ExitCode::FAILURE,
            };
            (serde_json::to_value(verification)?, code)
        }
        Err(err) => (error_json(&err), ExitCode::FAILURE),
    };
    println!("{}", serde_json::to_string_pretty(&printed)?);
    Ok(code)
}

fn expectations(
    outputs: &[String],
    network: &str,
    signers: Vec<String>,
) -> Result<Expectations, FluentError> {
    let outputs = outputs
        .iter()
        .map(|output| output.parse::<OutputExpectation>())
        .collect::<Result<_, _>>()?;
    let network = network
        .parse::<Network>()
        .map_err(|reason| FluentError::InvalidArguments {
            reason: format!("{reason}; expected mainnet, preprod or preview"),
            arguments: vec!["expect-network".to_string()],
            violations: Vec::new(),
        })?;
    Ok(Expectations {
        outputs,
        network,
        signers,
    })
}

/// The error as a tool result carries it: `{"error": {code, message,
/// details}}`, `details` only when there are some.
fn error_json(err: &FluentError) -> Value {
    let mut error = json!({ "code": err.code(), "message": err.message() });
    if let Some(details) = err.details() {
        error["details"] = details;
    }
    json!({ "error": error })
}

/// `cargo xtask transcript`: reads the logs, renders the transcript and says
/// on stderr how many entries it holds.
fn render_transcript(
    logs: &[PathBuf],
    sub_hashes: &[String],
    out: Option<PathBuf>,
) -> anyhow::Result<ExitCode> {
    let (text, source) = if logs.is_empty() {
        let text = std::io::read_to_string(std::io::stdin()).context("reading logs from stdin")?;
        (text, "stdin".to_string())
    } else {
        let mut text = String::new();
        for path in logs {
            let file = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            text.push_str(&file);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        let names = logs
            .iter()
            .map(|path| format!("`{}`", path.display()))
            .collect::<Vec<_>>();
        (text, names.join(", "))
    };
    let mut transcript = transcript::read(&text);
    if !sub_hashes.is_empty() {
        transcript.only(sub_hashes);
    }
    let markdown = transcript.to_markdown(&source);
    match &out {
        Some(path) => std::fs::write(path, &markdown)
            .with_context(|| format!("writing {}", path.display()))?,
        None => print!("{markdown}"),
    }
    eprintln!(
        "{} entries from {} log lines{}",
        transcript.entries.len(),
        transcript.lines,
        out.map(|path| format!(" written to {}", path.display()))
            .unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}
