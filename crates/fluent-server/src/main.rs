//! The `fluent` command-line entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use fluent_core::Config;
use tracing_subscriber::EnvFilter;

/// Tx3 Fluent: prepare Tx3 protocol transactions for agents and wallets.
#[derive(Debug, Parser)]
#[command(name = "fluent", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect Cardano addresses offline.
    Address {
        #[command(subcommand)]
        command: AddressCommand,
    },
    /// Inspect server configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AddressCommand {
    /// Decode an address and print its kind, network and credentials as JSON.
    Inspect {
        /// Bech32, hex or base58 (Byron) address.
        address: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Load and validate a configuration file, then print it with secrets
    /// redacted.
    Check {
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Address {
            command: AddressCommand::Inspect { address },
        } => {
            let report = fluent_core::address::inspect(&address)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Config {
            command: ConfigCommand::Check { config },
        } => {
            let loaded =
                Config::load(&config).with_context(|| format!("loading {}", config.display()))?;
            print!("{}", loaded.redacted());
            Ok(())
        }
    }
}
