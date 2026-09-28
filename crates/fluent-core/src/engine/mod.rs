//! The transaction preparation engine.
//!
//! [`Engine::prepare`] turns a [`PrepareRequest`] (a registration slug, a
//! transaction name and the caller's arguments) into a
//! [`PreparedTransaction`] or a classified [`FluentError`]:
//!
//! 1. find the registration (`unknown_protocol`), check that the server has a
//!    resolver for its network (`network_mismatch`), and find the
//!    transaction (`unknown_transaction`);
//! 2. check the arguments against the transaction's tool input schema and the
//!    values the deployment profile binds ([`arguments`]);
//! 3. resolve through the Rust SDK against the network's TRP endpoint, within
//!    `limits.resolver_timeout_secs` (`resolver_timeout`); resolver errors
//!    are classified as [`resolver`] describes;
//! 4. decode the returned CBOR with [`summary::decode`], which reads the CBOR
//!    only, and check the resolver's transaction hash against the hash of the
//!    body bytes (`internal` when they differ).
//!
//! The SDK client of each registration is built once, when the engine is,
//! against its network's endpoint, with the `dmtr-api-key` header when the
//! network's `trp_api_key_env` is set, and with the deployment profile
//! selected; so the SDK sends the profile's environment values as arguments.
//! A request clones that client and binds the caller's parties to it. Nothing
//! is signed or submitted.
//!
//! Nothing the engine logs contains an argument value or an API key.

pub mod arguments;
pub mod resolver;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tracing::{debug, warn};
use tx3_sdk::facade::{self, Party, ResolvedTx, Tx3Client};

use crate::config::{Config, NetworkConfig};
use crate::envelope::{PreparedTransaction, ProtocolRef};
use crate::error::FluentError;
use crate::registration::{Catalog, Registration};
use crate::summary;

pub use arguments::{ArgumentRules, CheckedArguments};
use resolver::Resolution;

/// The header that carries a network's TRP API key.
pub const API_KEY_HEADER: &str = "dmtr-api-key";

/// A request to prepare one transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct PrepareRequest {
    /// The registration slug.
    pub registration: String,
    /// The transaction name, as the TII spells it.
    pub tx: String,
    /// The caller's arguments: a JSON object keyed as the tool's input
    /// schema.
    pub args: Value,
}

/// Prepares unsigned transactions for the loaded registrations.
///
/// Built once at startup from the configuration's networks and limits and
/// the loaded [`Catalog`]; immutable afterwards.
pub struct Engine {
    registrations: BTreeMap<String, Slot>,
    /// The names of the networks the configuration has resolvers for.
    networks: Vec<String>,
    resolver_timeout: Duration,
}

/// What the engine holds for one registration.
enum Slot {
    Ready(Box<Target>),
    /// The configuration has no resolver for the registration's network.
    NetworkNotServed(String),
    /// Its SDK client or an input schema could not be built.
    Broken(String),
}

/// A registration the engine can prepare transactions for.
struct Target {
    registration: Arc<Registration>,
    client: Tx3Client,
    transactions: BTreeMap<String, ArgumentRules>,
}

impl Engine {
    /// Builds the engine: one SDK client per registration, against the
    /// resolver `config` defines for its network, and the argument rules of
    /// each of its transactions.
    ///
    /// A registration whose network has no resolver, or whose client or
    /// argument rules cannot be built, is kept and reported by
    /// [`Engine::prepare`] as `network_mismatch` or
    /// `registration_unavailable`; the others are unaffected.
    pub fn new(config: &Config, catalog: &Catalog) -> Engine {
        let registrations = catalog
            .iter()
            .map(|registration| {
                let slug = registration.slug().to_string();
                let network = registration.network().as_str();
                let slot = match config.networks.get(network) {
                    None => {
                        warn!(registration = %slug, %network, "no resolver configured for the registration's network");
                        Slot::NetworkNotServed(network.to_string())
                    }
                    Some(resolver) => match Target::new(Arc::clone(registration), resolver) {
                        Ok(target) => Slot::Ready(Box::new(target)),
                        Err(reason) => {
                            warn!(registration = %slug, %reason, "registration unavailable");
                            Slot::Broken(reason)
                        }
                    },
                };
                (slug, slot)
            })
            .collect();

        Engine {
            registrations,
            networks: config.networks.keys().cloned().collect(),
            resolver_timeout: Duration::from_secs(config.limits.resolver_timeout_secs),
        }
    }

    /// Prepares the transaction `request` names; see the [module
    /// documentation](self) for the steps and their errors.
    pub async fn prepare(
        &self,
        request: PrepareRequest,
    ) -> Result<PreparedTransaction, FluentError> {
        let PrepareRequest {
            registration: slug,
            tx,
            args,
        } = request;
        let target = match self.registrations.get(&slug) {
            None => return Err(FluentError::UnknownProtocol { protocol: slug }),
            Some(Slot::NetworkNotServed(network)) => {
                return Err(FluentError::NetworkMismatch {
                    requested: network.clone(),
                    available: self.networks.clone(),
                    registration: None,
                });
            }
            Some(Slot::Broken(reason)) => {
                return Err(FluentError::RegistrationUnavailable {
                    registration: slug,
                    reason: reason.clone(),
                });
            }
            Some(Slot::Ready(target)) => target,
        };
        let Some(rules) = target.transactions.get(&tx) else {
            return Err(FluentError::UnknownTransaction {
                protocol: slug,
                transaction: tx,
            });
        };
        let checked = rules.check(&args)?;

        let network = target.registration.network().as_str();
        let resolution = Resolution {
            registration: &slug,
            network,
        };
        debug!(registration = %slug, %tx, %network, "resolving transaction");
        let resolved = tokio::time::timeout(self.resolver_timeout, target.resolve(&tx, checked))
            .await
            .map_err(|_| {
                warn!(registration = %slug, %network, "resolver timed out");
                FluentError::ResolverTimeout {
                    timeout_secs: self.resolver_timeout.as_secs(),
                }
            })?
            .map_err(|error| resolver::classify(error, resolution))?;

        let decoded = summary::decode(&resolved.tx_hex)?;
        if !decoded.tx_hash.eq_ignore_ascii_case(&resolved.hash) {
            warn!(
                registration = %slug,
                resolver_tx_hash = %resolved.hash,
                computed_tx_hash = %decoded.tx_hash,
                "resolver transaction hash differs from the body hash"
            );
            return Err(FluentError::internal_with_details(
                "the resolver's transaction hash differs from the hash of the body it returned",
                json!({
                    "resolver_tx_hash": resolved.hash,
                    "computed_tx_hash": decoded.tx_hash,
                }),
            ));
        }
        debug!(registration = %slug, %tx, tx_hash = %decoded.tx_hash, "prepared transaction");

        let registration = &target.registration;
        let protocol = &registration.manifest().protocol;
        Ok(PreparedTransaction::new(
            ProtocolRef {
                scope: protocol.scope.clone(),
                name: protocol.name.clone(),
                version: protocol.version.clone(),
                registration_slug: slug,
                registration_revision: registration.revision().to_string(),
                tii_digest: registration.tii_digest().to_string(),
            },
            tx,
            network,
            decoded.tx_hash,
            resolved.tx_hex,
            decoded.transaction,
        ))
    }
}

impl Target {
    /// Builds the SDK client and every transaction's argument rules, or says
    /// why they cannot be built.
    fn new(registration: Arc<Registration>, resolver: &NetworkConfig) -> Result<Target, String> {
        let mut builder = registration
            .protocol()
            .clone()
            .client()
            .trp_endpoint(&resolver.trp_url);
        if let Some(key) = resolver
            .trp_api_key_env
            .as_ref()
            .and_then(|env| env.value())
        {
            builder = builder.with_header(API_KEY_HEADER, key);
        }
        let client = builder
            .with_profile(registration.profile())
            .build()
            .map_err(|err| format!("cannot build the SDK client: {err}"))?;

        let transactions = registration
            .tii()
            .transactions
            .keys()
            .map(|tx| {
                ArgumentRules::new(registration.tii(), tx, registration.profile())
                    .map(|rules| (tx.clone(), rules))
            })
            .collect::<Result<_, _>>()?;

        Ok(Target {
            registration,
            client,
            transactions,
        })
    }

    /// Resolves `tx` with the caller's parties bound and `checked.args` as
    /// arguments.
    async fn resolve(
        &self,
        tx: &str,
        checked: CheckedArguments,
    ) -> Result<ResolvedTx, facade::Error> {
        let mut client = self.client.clone();
        for (name, address) in checked.parties {
            client = client.with_party(name, Party::address(address))?;
        }
        client.tx(tx)?.args(checked.args).resolve().await
    }
}
