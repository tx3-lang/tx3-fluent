//! The `fluent` command-line entry point.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};
use fluent_core::Config;
use fluent_core::registration::{self, Loaded};
use serde::Serialize;
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
    /// Inspect server configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Inspect protocol registrations.
    Registrations {
        #[command(subcommand)]
        command: RegistrationsCommand,
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

#[derive(Debug, Subcommand)]
enum RegistrationsCommand {
    /// Load every bundle in `[registrations].dir`, fetching registry-sourced
    /// TII, and print each registration, or why its bundle was rejected. Fails
    /// when any bundle is rejected.
    Check {
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Config {
            command: ConfigCommand::Check { config },
        } => {
            let loaded =
                Config::load(&config).with_context(|| format!("loading {}", config.display()))?;
            print!("{}", loaded.redacted());
            Ok(())
        }
        Command::Registrations {
            command: RegistrationsCommand::Check { config },
        } => {
            let loaded =
                Config::load(&config).with_context(|| format!("loading {}", config.display()))?;
            let dir = &loaded.registrations.dir;
            let registrations = registration::load_dir(dir).await?;
            print!("{}", RegistrationsReport::new(&registrations).to_toml()?);

            let total = registrations.catalog.len() + registrations.rejected.len();
            if total == 0 {
                eprintln!("no registration bundles in {}", dir.display());
            }
            if !registrations.rejected.is_empty() {
                anyhow::bail!(
                    "{} of {total} registration bundles rejected",
                    registrations.rejected.len()
                );
            }
            Ok(())
        }
    }
}

/// What `registrations check` prints, as TOML.
#[derive(Serialize)]
struct RegistrationsReport {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    registration: Vec<RegistrationRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rejected: Vec<RejectedRow>,
}

#[derive(Serialize)]
struct RegistrationRow {
    slug: String,
    protocol: String,
    network: String,
    profile: String,
    revision: String,
    tii_digest: String,
    skill_digest: String,
    bundle: String,
    /// Registry-sourced registrations only; last, because TOML writes tables
    /// after plain values.
    #[serde(skip_serializing_if = "Option::is_none")]
    provenance: Option<ProvenanceRow>,
}

/// Where a registry-sourced TII came from. Recorded, not a verified
/// publisher identity.
#[derive(Serialize)]
struct ProvenanceRow {
    registry_url: String,
    #[serde(rename = "ref")]
    reference: String,
    manifest_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_revision: Option<String>,
}

#[derive(Serialize)]
struct RejectedRow {
    bundle: String,
    code: &'static str,
    message: String,
}

impl RegistrationsReport {
    fn new(loaded: &Loaded) -> Self {
        let registration = loaded
            .catalog
            .iter()
            .map(|r| RegistrationRow {
                slug: r.slug().to_string(),
                protocol: r.protocol_id(),
                network: r.network().to_string(),
                profile: r.profile().to_string(),
                revision: r.revision().to_string(),
                tii_digest: r.tii_digest().to_string(),
                skill_digest: r.skill_digest().to_string(),
                bundle: r.bundle().display().to_string(),
                provenance: r.provenance().map(|p| ProvenanceRow {
                    registry_url: p.registry_url.clone(),
                    reference: p.reference.clone(),
                    manifest_digest: p.manifest_digest.clone(),
                    source_digest: p.source_digest.clone(),
                    source_revision: p.source_revision.clone(),
                }),
            })
            .collect();
        let rejected = loaded
            .rejected
            .iter()
            .map(|r| RejectedRow {
                bundle: r.bundle.display().to_string(),
                code: r.error.code().as_str(),
                message: r.error.message(),
            })
            .collect();
        RegistrationsReport {
            registration,
            rejected,
        }
    }

    fn to_toml(&self) -> anyhow::Result<String> {
        toml::to_string(self).context("rendering the registrations report")
    }
}
