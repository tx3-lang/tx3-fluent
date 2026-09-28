//! The error every Fluent operation reports to callers.
//!
//! A [`FluentError`] has three views: a stable machine-readable [`ErrorCode`],
//! a human [`message`](FluentError::message) and optional structured
//! [`details`](FluentError::details). Variants only carry context that is safe
//! to hand back to a caller or write to a log: identifiers, argument *names*,
//! limits and validator traces. They never carry credentials or complete
//! argument values, so no view can leak them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Stable, machine-readable classification of a [`FluentError`].
///
/// Serializes as its snake_case name (for example `"invalid_arguments"`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The caller's arguments are missing, malformed or out of range.
    InvalidArguments,
    /// The protocol exists but has no transaction with the requested name.
    UnknownTransaction,
    /// No registered protocol matches the request.
    UnknownProtocol,
    /// The registration exists but its artifacts cannot be used right now.
    RegistrationUnavailable,
    /// The request targets a network the protocol or server does not serve.
    NetworkMismatch,
    /// The wallet cannot cover the value the transaction needs.
    InsufficientFunds,
    /// The resolver could not find UTxOs matching a transaction input.
    InputNotResolved,
    /// A validator script rejected the transaction.
    ScriptFailure,
    /// The resolver did not answer within the configured timeout.
    ResolverTimeout,
    /// The resolver could not be reached or failed unexpectedly.
    ResolverUnavailable,
    /// The caller has used their request quota.
    QuotaExhausted,
    /// The caller is not authenticated or not allowed to make the request.
    Unauthorized,
    /// An unexpected server-side failure.
    Internal,
}

impl ErrorCode {
    /// Every code, in declaration order.
    pub const ALL: [ErrorCode; 13] = [
        ErrorCode::InvalidArguments,
        ErrorCode::UnknownTransaction,
        ErrorCode::UnknownProtocol,
        ErrorCode::RegistrationUnavailable,
        ErrorCode::NetworkMismatch,
        ErrorCode::InsufficientFunds,
        ErrorCode::InputNotResolved,
        ErrorCode::ScriptFailure,
        ErrorCode::ResolverTimeout,
        ErrorCode::ResolverUnavailable,
        ErrorCode::QuotaExhausted,
        ErrorCode::Unauthorized,
        ErrorCode::Internal,
    ];

    /// The code's wire name, identical to its serialized form.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidArguments => "invalid_arguments",
            ErrorCode::UnknownTransaction => "unknown_transaction",
            ErrorCode::UnknownProtocol => "unknown_protocol",
            ErrorCode::RegistrationUnavailable => "registration_unavailable",
            ErrorCode::NetworkMismatch => "network_mismatch",
            ErrorCode::InsufficientFunds => "insufficient_funds",
            ErrorCode::InputNotResolved => "input_not_resolved",
            ErrorCode::ScriptFailure => "script_failure",
            ErrorCode::ResolverTimeout => "resolver_timeout",
            ErrorCode::ResolverUnavailable => "resolver_unavailable",
            ErrorCode::QuotaExhausted => "quota_exhausted",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::Internal => "internal",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One way the caller's arguments break a transaction's input schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Violation {
    /// JSON Pointer to the offending value within the arguments, such as
    /// `/quantity`; empty for the arguments object itself.
    pub path: String,
    /// What is wrong, phrased without quoting the value.
    pub message: String,
}

/// What the resolver reported about a transaction input it could not
/// resolve, reduced to counts: never an address, an amount or a UTxO.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputDiagnostic {
    /// Whether the input's query named an address.
    pub has_address: bool,
    /// How many UTxO references the query named.
    pub refs: usize,
    /// How many UTxOs of the search space matched the query.
    pub matched: usize,
}

impl InputDiagnostic {
    fn details(input: Option<&String>, diagnostic: Option<&InputDiagnostic>) -> Option<Value> {
        if input.is_none() && diagnostic.is_none() {
            return None;
        }
        let mut details = json!({});
        if let Some(input) = input {
            details["input"] = json!(input);
        }
        if let Some(diagnostic) = diagnostic {
            details["query"] = json!({
                "has_address": diagnostic.has_address,
                "refs": diagnostic.refs,
            });
            details["search_space"] = json!({ "matched": diagnostic.matched });
        }
        Some(details)
    }
}

/// A failure reported to a Fluent caller.
///
/// Construct variants with identifiers and names only; never place a credential
/// or a complete argument value in any field.
#[derive(Debug, thiserror::Error)]
pub enum FluentError {
    /// The caller's arguments are missing, malformed or out of range.
    #[error("invalid arguments: {reason}")]
    InvalidArguments {
        /// What is wrong, phrased without quoting argument values.
        reason: String,
        /// Names of the offending arguments.
        arguments: Vec<String>,
        /// Each way the arguments break the input schema; may be empty.
        violations: Vec<Violation>,
    },

    /// The protocol exists but has no transaction with the requested name.
    #[error("protocol {protocol} has no transaction named {transaction}")]
    UnknownTransaction {
        /// The protocol that was searched.
        protocol: String,
        /// The transaction name that was requested.
        transaction: String,
    },

    /// No registered protocol matches the request.
    #[error("no registered protocol named {protocol}")]
    UnknownProtocol {
        /// The protocol that was requested.
        protocol: String,
    },

    /// The registration exists but its artifacts cannot be used right now.
    #[error("registration {registration} is unavailable: {reason}")]
    RegistrationUnavailable {
        /// The registration slug.
        registration: String,
        /// Why it cannot be used.
        reason: String,
    },

    /// The request targets a network the protocol or server does not serve,
    /// or a registration declares a network its deployment profile does not
    /// serve.
    #[error("{}", network_mismatch_message(.requested, .available, .registration.as_deref()))]
    NetworkMismatch {
        /// The network the caller asked for, or the registration declares.
        requested: String,
        /// The networks that would have been accepted.
        available: Vec<String>,
        /// The registration bundle that declares `requested`, when the
        /// mismatch is in a registration rather than a request.
        registration: Option<String>,
    },

    /// The wallet cannot cover the value the transaction needs.
    #[error("insufficient funds to build the transaction")]
    InsufficientFunds {
        /// The transaction input that could not be funded, when known.
        input: Option<String>,
        /// What the resolver reported about the input, when it did.
        diagnostic: Option<InputDiagnostic>,
    },

    /// The resolver could not find UTxOs matching a transaction input.
    #[error("the resolver found no UTxOs for a transaction input")]
    InputNotResolved {
        /// The transaction input that could not be resolved, when known.
        input: Option<String>,
        /// What the resolver reported about the input, when it did.
        diagnostic: Option<InputDiagnostic>,
    },

    /// A validator script rejected the transaction.
    #[error("a validator script rejected the transaction")]
    ScriptFailure {
        /// Validator trace messages as reported by the resolver.
        logs: Vec<String>,
    },

    /// The resolver did not answer within the configured timeout.
    #[error("the resolver did not answer within {timeout_secs}s")]
    ResolverTimeout {
        /// The timeout that elapsed, in seconds.
        timeout_secs: u64,
    },

    /// The resolver could not be reached or failed unexpectedly.
    #[error("the resolver for network {network} is unavailable")]
    ResolverUnavailable {
        /// The network whose resolver failed.
        network: String,
        /// The HTTP status the resolver answered with, when it answered.
        status: Option<u16>,
    },

    /// The caller has used their request quota.
    #[error("request quota of {limit} per day is exhausted")]
    QuotaExhausted {
        /// The daily limit that was reached.
        limit: u32,
    },

    /// The caller is not authenticated or not allowed to make the request.
    #[error("unauthorized")]
    Unauthorized,

    /// An unexpected server-side failure. The source is kept for logs and is
    /// never shown to callers.
    #[error("internal error")]
    Internal {
        /// The underlying failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        /// Identifiers that help report the failure, such as two transaction
        /// hashes that should have been equal; never the source's text.
        details: Option<Value>,
    },
}

fn network_mismatch_message(
    requested: &str,
    available: &[String],
    registration: Option<&str>,
) -> String {
    match registration {
        None => format!("network {requested} is not available for this request"),
        Some(registration) => format!(
            "registration {registration} declares network {requested}, \
             but its deployment profile serves {}",
            available.join(", ")
        ),
    }
}

impl FluentError {
    /// Wraps an unexpected failure as [`FluentError::Internal`].
    pub fn internal(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        FluentError::Internal {
            source: source.into(),
            details: None,
        }
    }

    /// Wraps an unexpected failure as [`FluentError::Internal`] with
    /// `details` for the caller. `details` must hold identifiers only.
    pub fn internal_with_details(
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
        details: Value,
    ) -> Self {
        FluentError::Internal {
            source: source.into(),
            details: Some(details),
        }
    }

    /// The stable code for this error.
    pub fn code(&self) -> ErrorCode {
        match self {
            FluentError::InvalidArguments { .. } => ErrorCode::InvalidArguments,
            FluentError::UnknownTransaction { .. } => ErrorCode::UnknownTransaction,
            FluentError::UnknownProtocol { .. } => ErrorCode::UnknownProtocol,
            FluentError::RegistrationUnavailable { .. } => ErrorCode::RegistrationUnavailable,
            FluentError::NetworkMismatch { .. } => ErrorCode::NetworkMismatch,
            FluentError::InsufficientFunds { .. } => ErrorCode::InsufficientFunds,
            FluentError::InputNotResolved { .. } => ErrorCode::InputNotResolved,
            FluentError::ScriptFailure { .. } => ErrorCode::ScriptFailure,
            FluentError::ResolverTimeout { .. } => ErrorCode::ResolverTimeout,
            FluentError::ResolverUnavailable { .. } => ErrorCode::ResolverUnavailable,
            FluentError::QuotaExhausted { .. } => ErrorCode::QuotaExhausted,
            FluentError::Unauthorized => ErrorCode::Unauthorized,
            FluentError::Internal { .. } => ErrorCode::Internal,
        }
    }

    /// A sentence for humans. Never includes the source of an internal error.
    pub fn message(&self) -> String {
        self.to_string()
    }

    /// Structured context for callers, or `None` when there is nothing to add.
    ///
    /// Contains identifiers, argument names, limits and validator traces only;
    /// never credentials or complete argument values.
    pub fn details(&self) -> Option<Value> {
        match self {
            FluentError::InvalidArguments {
                arguments,
                violations,
                ..
            } => {
                let mut details = json!({ "arguments": arguments });
                if !violations.is_empty() {
                    details["violations"] = json!(violations);
                }
                Some(details)
            }
            FluentError::UnknownTransaction {
                protocol,
                transaction,
            } => Some(json!({ "protocol": protocol, "transaction": transaction })),
            FluentError::UnknownProtocol { protocol } => Some(json!({ "protocol": protocol })),
            FluentError::RegistrationUnavailable { registration, .. } => {
                Some(json!({ "registration": registration }))
            }
            FluentError::NetworkMismatch {
                requested,
                available,
                registration,
            } => {
                let mut details = json!({ "requested": requested, "available": available });
                if let Some(registration) = registration {
                    details["registration"] = json!(registration);
                }
                Some(details)
            }
            FluentError::InsufficientFunds { input, diagnostic }
            | FluentError::InputNotResolved { input, diagnostic } => {
                InputDiagnostic::details(input.as_ref(), diagnostic.as_ref())
            }
            FluentError::ScriptFailure { logs } => Some(json!({ "logs": logs })),
            FluentError::ResolverTimeout { timeout_secs } => {
                Some(json!({ "timeout_secs": timeout_secs }))
            }
            FluentError::ResolverUnavailable { network, status } => {
                let mut details = json!({ "network": network });
                if let Some(status) = status {
                    details["status"] = json!(status);
                }
                Some(details)
            }
            FluentError::QuotaExhausted { limit } => Some(json!({ "limit": limit })),
            FluentError::Internal { details, .. } => details.clone(),
            FluentError::Unauthorized => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn one_of_each() -> Vec<FluentError> {
        vec![
            FluentError::InvalidArguments {
                reason: "missing required argument".into(),
                arguments: vec!["quantity".into()],
                violations: vec![],
            },
            FluentError::UnknownTransaction {
                protocol: "acme/swap".into(),
                transaction: "burn".into(),
            },
            FluentError::UnknownProtocol {
                protocol: "acme/nothing".into(),
            },
            FluentError::RegistrationUnavailable {
                registration: "acme-swap".into(),
                reason: "digest mismatch".into(),
            },
            FluentError::NetworkMismatch {
                requested: "mainnet".into(),
                available: vec!["preprod".into()],
                registration: None,
            },
            FluentError::InsufficientFunds {
                input: Some("source".into()),
                diagnostic: None,
            },
            FluentError::InputNotResolved {
                input: None,
                diagnostic: None,
            },
            FluentError::ScriptFailure {
                logs: vec!["deadline passed".into()],
            },
            FluentError::ResolverTimeout { timeout_secs: 30 },
            FluentError::ResolverUnavailable {
                network: "preprod".into(),
                status: None,
            },
            FluentError::QuotaExhausted { limit: 200 },
            FluentError::Unauthorized,
            FluentError::internal("disk on fire"),
        ]
    }

    #[test]
    fn every_variant_maps_to_a_distinct_code() {
        let codes: BTreeSet<ErrorCode> = one_of_each().iter().map(FluentError::code).collect();
        let all: BTreeSet<ErrorCode> = ErrorCode::ALL.into_iter().collect();
        assert_eq!(codes, all);
    }

    #[test]
    fn codes_use_the_contract_names() {
        let names: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "invalid_arguments",
                "unknown_transaction",
                "unknown_protocol",
                "registration_unavailable",
                "network_mismatch",
                "insufficient_funds",
                "input_not_resolved",
                "script_failure",
                "resolver_timeout",
                "resolver_unavailable",
                "quota_exhausted",
                "unauthorized",
                "internal",
            ]
        );
        for code in ErrorCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), json!(code.as_str()));
            assert_eq!(code.to_string(), code.as_str());
        }
    }

    #[test]
    fn internal_errors_hide_their_source() {
        let err = FluentError::internal("connection string postgres://user:pw@db");
        assert_eq!(err.message(), "internal error");
        assert_eq!(err.details(), None);
    }

    #[test]
    fn invalid_arguments_details_carry_names_only() {
        let err = FluentError::InvalidArguments {
            reason: "not a valid address".into(),
            arguments: vec!["receiver".into()],
            violations: vec![],
        };
        assert_eq!(err.code(), ErrorCode::InvalidArguments);
        assert_eq!(err.message(), "invalid arguments: not a valid address");
        assert_eq!(err.details(), Some(json!({ "arguments": ["receiver"] })));

        let err = FluentError::InvalidArguments {
            reason: "value is not of type \"integer\" at /quantity".into(),
            arguments: vec!["quantity".into()],
            violations: vec![Violation {
                path: "/quantity".into(),
                message: "value is not of type \"integer\"".into(),
            }],
        };
        assert_eq!(
            err.details(),
            Some(json!({
                "arguments": ["quantity"],
                "violations": [{ "path": "/quantity", "message": "value is not of type \"integer\"" }]
            }))
        );
    }

    #[test]
    fn unresolved_inputs_report_counts_only() {
        let diagnostic = InputDiagnostic {
            has_address: true,
            refs: 0,
            matched: 3,
        };
        let err = FluentError::InsufficientFunds {
            input: Some("source".into()),
            diagnostic: Some(diagnostic),
        };
        assert_eq!(
            err.details(),
            Some(json!({
                "input": "source",
                "query": { "has_address": true, "refs": 0 },
                "search_space": { "matched": 3 }
            }))
        );
        let err = FluentError::InputNotResolved {
            input: None,
            diagnostic: None,
        };
        assert_eq!(err.details(), None);
    }

    #[test]
    fn resolver_unavailable_carries_the_status_only_when_known() {
        let err = FluentError::ResolverUnavailable {
            network: "preprod".into(),
            status: Some(503),
        };
        assert_eq!(
            err.details(),
            Some(json!({ "network": "preprod", "status": 503 }))
        );
        let err = FluentError::ResolverUnavailable {
            network: "preprod".into(),
            status: None,
        };
        assert_eq!(err.details(), Some(json!({ "network": "preprod" })));
    }

    #[test]
    fn internal_details_are_explicit() {
        let err = FluentError::internal_with_details(
            "hash mismatch in resolver output",
            json!({ "resolver_tx_hash": "aa", "computed_tx_hash": "bb" }),
        );
        assert_eq!(err.code(), ErrorCode::Internal);
        assert_eq!(err.message(), "internal error");
        assert_eq!(
            err.details(),
            Some(json!({ "resolver_tx_hash": "aa", "computed_tx_hash": "bb" }))
        );
    }

    #[test]
    fn network_mismatch_names_the_registration_when_there_is_one() {
        let request = FluentError::NetworkMismatch {
            requested: "mainnet".into(),
            available: vec!["preprod".into()],
            registration: None,
        };
        assert_eq!(
            request.message(),
            "network mainnet is not available for this request"
        );
        assert_eq!(
            request.details(),
            Some(json!({ "requested": "mainnet", "available": ["preprod"] }))
        );

        let bundle = FluentError::NetworkMismatch {
            requested: "preprod".into(),
            available: vec!["mainnet".into()],
            registration: Some("registrations/strike".into()),
        };
        assert_eq!(bundle.code(), ErrorCode::NetworkMismatch);
        assert_eq!(
            bundle.message(),
            "registration registrations/strike declares network preprod, \
             but its deployment profile serves mainnet"
        );
        assert_eq!(
            bundle.details(),
            Some(json!({
                "requested": "preprod",
                "available": ["mainnet"],
                "registration": "registrations/strike"
            }))
        );
    }
}
