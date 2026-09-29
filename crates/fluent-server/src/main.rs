//! The `fluent` command-line entry point.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::{ArgGroup, Parser, Subcommand};
use fluent_core::config::AuthConfig;
use fluent_core::registration::{self, Loaded};
use fluent_core::{Config, Engine, FluentError, PrepareRequest};
use fluent_server::http::{HttpServer, shutdown_signal};
use fluent_server::limits::{Gate, Limits, Quota};
use fluent_server::logging;
use fluent_server::mcp::{AllRegistrations, FluentHandler, Scoping, error_json};
use fluent_server::site::Site;
use fluent_server::store::{Store, UserScopes};
use rmcp::ServiceExt;
use serde::Serialize;
use serde_json::Value;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

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
    /// Inspect protocol registrations.
    Registrations {
        #[command(subcommand)]
        command: RegistrationsCommand,
    },
    /// Prepare one unsigned transaction and print its envelope as JSON, or
    /// the error as `{"error": {code, message, details}}` with a non-zero
    /// exit status. Nothing is signed or submitted.
    Prepare {
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
        /// The registration slug.
        #[arg(long, value_name = "SLUG")]
        registration: String,
        /// The transaction name, as the TII spells it.
        #[arg(long, value_name = "NAME")]
        tx: String,
        /// The arguments, as a JSON object keyed as the transaction's tool
        /// input schema.
        #[arg(long, value_name = "JSON")]
        args: String,
    },
    /// Serve the loaded registrations' tools over MCP. Logs go to stderr as
    /// JSON lines. Over HTTP with `oidc` authentication and a `[store]`, each
    /// user sees the registrations they selected, and chooses them on the site
    /// when `[site].enabled`; otherwise every session sees every registration.
    /// Transaction tools run under `[limits]`; over HTTP with a `[store]`, each
    /// principal's daily quota is enforced.
    #[command(group(ArgGroup::new("transport").required(true).args(["stdio", "http"])))]
    Serve {
        /// Speak MCP over stdin and stdout.
        #[arg(long)]
        stdio: bool,
        /// Speak MCP over Streamable HTTP at `/mcp` on `[server].listen`,
        /// authenticating callers as `[auth]` says, until SIGTERM or Ctrl-C.
        #[arg(long)]
        http: bool,
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
    },
    /// Operate on the users in the `[store]` database.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Revoke a user: they see no tools and every call fails as
    /// `unauthorized`, including in sessions already open. Prints the user
    /// as JSON.
    Revoke {
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
        /// The user's OIDC subject.
        #[arg(long, value_name = "SUB")]
        sub: String,
    },
    /// Print a user and the registrations they selected as JSON.
    Selections {
        /// Configuration file to load; `FLUENT_*` environment overrides apply.
        #[arg(long, value_name = "FILE")]
        config: PathBuf,
        /// The user's OIDC subject.
        #[arg(long, value_name = "SUB")]
        sub: String,
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

    if matches!(cli.command, Command::Serve { .. }) {
        // A server logs its lifecycle unless `RUST_LOG` says otherwise, and
        // never a message body.
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let logs = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(std::io::stderr)
            .with_filter(filter)
            .with_filter(logging::no_message_bodies());
        tracing_subscriber::registry().with(logs).init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_env_filter(EnvFilter::from_default_env())
            .init();
    }

    match run(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Address {
            command: AddressCommand::Inspect { address },
        } => {
            let report = fluent_core::address::inspect(&address)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Config {
            command: ConfigCommand::Check { config },
        } => {
            let loaded =
                Config::load(&config).with_context(|| format!("loading {}", config.display()))?;
            print!("{}", loaded.redacted());
            Ok(ExitCode::SUCCESS)
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
            Ok(ExitCode::SUCCESS)
        }
        Command::Prepare {
            config,
            registration,
            tx,
            args,
        } => prepare(&config, registration, tx, &args).await,
        Command::Serve { http, config, .. } => serve(&config, http).await,
        Command::Admin { command } => admin(command).await,
    }
}

/// `fluent admin`: opens the configured store and revokes or prints a user.
async fn admin(command: AdminCommand) -> anyhow::Result<ExitCode> {
    let (AdminCommand::Revoke { config, sub } | AdminCommand::Selections { config, sub }) =
        &command;
    let store = open_store(config).await?;
    let printed = match command {
        AdminCommand::Revoke { .. } => serde_json::to_value(store.revoke(sub).await?)?,
        AdminCommand::Selections { .. } => serde_json::json!({
            "user": store.user(sub).await?,
            "selections": store.list_selections(sub).await?,
        }),
    };
    println!("{}", serde_json::to_string_pretty(&printed)?);
    Ok(ExitCode::SUCCESS)
}

/// Opens the `[store]` database `config` names.
async fn open_store(config: &Path) -> anyhow::Result<Store> {
    let loaded = Config::load(config).with_context(|| format!("loading {}", config.display()))?;
    let Some(store) = &loaded.store else {
        anyhow::bail!("{} has no [store] table", config.display());
    };
    Store::open(&store.sqlite_path).await
}

/// `fluent serve`: loads the configuration and registrations, then serves
/// MCP on stdin and stdout until the client closes stdin, or over HTTP when
/// `http` until SIGTERM or Ctrl-C.
async fn serve(config: &Path, http: bool) -> anyhow::Result<ExitCode> {
    let loaded = Config::load(config).with_context(|| format!("loading {}", config.display()))?;
    let registrations = registration::load_dir(&loaded.registrations.dir).await?;
    for rejected in &registrations.rejected {
        tracing::warn!(
            bundle = %rejected.bundle.display(),
            code = %rejected.error.code(),
            "registration bundle rejected: {}",
            rejected.error.message()
        );
    }
    let catalog = Arc::new(registrations.catalog);
    let engine = Arc::new(Engine::new(&loaded, &catalog));
    let store = match &loaded.store {
        Some(store) if http => Some(Store::open(&store.sqlite_path).await?),
        _ => None,
    };
    // The site shares the store with the scopes, so its selection changes
    // reach open sessions as `tools/list_changed`.
    let (scoping, site): (Arc<dyn Scoping>, Option<Site>) = match (&store, &loaded.auth) {
        (Some(store), AuthConfig::Oidc { .. }) => {
            tracing::info!("each user sees the registrations they selected");
            let site = Site::new(&loaded, store.clone(), Arc::clone(&catalog))
                .context("configuring the site")?;
            let scoping = Arc::new(UserScopes::new(store.clone(), Arc::clone(&catalog)));
            (scoping, site)
        }
        _ => (Arc::new(AllRegistrations::new(&catalog)), None),
    };
    let limits = &loaded.limits;
    let quota = store.map(|store| {
        tracing::info!(
            per_user_daily_quota = limits.per_user_daily_quota,
            "each principal's daily quota is enforced"
        );
        Quota::new(store, limits.per_user_daily_quota)
    });
    let handler = FluentHandler::new(Arc::clone(&catalog), engine, scoping)
        .context("building the tool catalog")?
        .with_limits(Limits {
            gate: Gate::from_limits(limits),
            quota,
        });
    let tools = handler.tools().len();

    if http {
        let site_enabled = site.is_some();
        let server = HttpServer::bind_with_site(&loaded, handler, site).await?;
        tracing::info!(
            registrations = catalog.len(),
            tools,
            site = site_enabled,
            address = %server.local_addr()?,
            "serving MCP over HTTP"
        );
        server.run(shutdown_signal()).await?;
        tracing::info!("HTTP server stopped");
        return Ok(ExitCode::SUCCESS);
    }
    tracing::info!(
        registrations = catalog.len(),
        tools,
        "serving MCP over stdio"
    );
    let service = handler
        .serve(rmcp::transport::stdio())
        .await
        .context("starting the MCP session")?;
    let reason = service.waiting().await.context("serving MCP")?;
    tracing::info!(?reason, "MCP session ended");
    Ok(ExitCode::SUCCESS)
}

/// `fluent prepare`: loads the configuration and registrations, prepares one
/// transaction and prints the envelope or the error as JSON on stdout.
async fn prepare(
    config: &Path,
    registration: String,
    tx: String,
    args: &str,
) -> anyhow::Result<ExitCode> {
    let loaded = Config::load(config).with_context(|| format!("loading {}", config.display()))?;
    let registrations = registration::load_dir(&loaded.registrations.dir).await?;
    for rejected in &registrations.rejected {
        eprintln!(
            "warning: registration bundle {} rejected: {}",
            rejected.bundle.display(),
            rejected.error.message()
        );
    }

    let result = match serde_json::from_str::<Value>(args) {
        // The parser's message gives a position, never the text.
        Err(err) => Err(FluentError::InvalidArguments {
            reason: format!("--args is not valid JSON: {err}"),
            arguments: Vec::new(),
            violations: Vec::new(),
        }),
        Ok(args) => {
            let engine = Engine::new(&loaded, &registrations.catalog);
            engine
                .prepare(PrepareRequest {
                    registration,
                    tx,
                    args,
                })
                .await
        }
    };

    let (printed, code) = match result {
        Ok(prepared) => (serde_json::to_value(prepared)?, ExitCode::SUCCESS),
        Err(err) => (error_json(&err), ExitCode::FAILURE),
    };
    println!("{}", serde_json::to_string_pretty(&printed)?);
    Ok(code)
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
