//! Classifying what the Rust SDK reports while resolving a transaction.
//!
//! | SDK error | Fluent code | Details |
//! | --- | --- | --- |
//! | `InputNotResolved`, query with a `min_amount` | `insufficient_funds` | input name; query address presence and ref count; matched count |
//! | `InputNotResolved`, query without one | `input_not_resolved` | as above |
//! | `TxScriptFailure` | `script_failure` | the first [`SCRIPT_LOG_LINES`] log lines |
//! | `MissingTxArg` | `invalid_arguments` | the argument name |
//! | `UnsupportedTir` | `registration_unavailable` | |
//! | `NetworkError`, `HttpError` | `resolver_unavailable` | the HTTP status, when there is one |
//! | facade `UnknownTx` | `unknown_transaction` | |
//!
//! The rows above are the contract. The remaining SDK errors are classified
//! by who can fix them: a template the resolver cannot read
//! (`InvalidTirEnvelope`, `InvalidTirBytes`, `UnsupportedTxEra`) is
//! `registration_unavailable`; a reply that is not a TRP answer
//! (`DeserializationError`, `GenericRpcError`, `UnknownError`,
//! `UnsupportedEra`) is `resolver_unavailable`; an argument the SDK cannot
//! encode or a party it does not know is `invalid_arguments`; anything else
//! is `internal`. A resolver timeout is classified by the engine, which owns
//! the clock.
//!
//! Only names, counts, codes and statuses are logged or returned: never an
//! address, an amount, an argument value or a resolver message.

use tracing::{debug, warn};
use tx3_sdk::facade;
use tx3_sdk::tii;
use tx3_sdk::trp;

use crate::error::{FluentError, InputDiagnostic, Violation};

/// How many validator log lines a `script_failure` keeps.
pub const SCRIPT_LOG_LINES: usize = 20;

/// Where the failing resolution was sent.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Resolution<'a> {
    /// The registration slug.
    pub registration: &'a str,
    /// The network whose resolver was called.
    pub network: &'a str,
}

/// Classifies an SDK error raised while resolving for `resolution`.
pub(crate) fn classify(error: facade::Error, resolution: Resolution<'_>) -> FluentError {
    let Resolution {
        registration,
        network,
    } = resolution;
    match error {
        facade::Error::Trp(error) => classify_trp(error, resolution),
        facade::Error::UnknownTx(tx) | facade::Error::Tii(tii::Error::UnknownTx(tx)) => {
            FluentError::UnknownTransaction {
                protocol: registration.to_string(),
                transaction: tx,
            }
        }
        facade::Error::UnknownParty(party) => FluentError::InvalidArguments {
            reason: format!("unknown party `{party}`"),
            violations: vec![Violation {
                path: super::arguments::pointer(&party),
                message: "unknown party".to_string(),
            }],
            arguments: vec![party],
        },
        facade::Error::Tii(tii::Error::EncodeArg(error)) => FluentError::InvalidArguments {
            reason: format!("an argument does not match its declared type: {error}"),
            arguments: Vec::new(),
            violations: vec![Violation {
                path: String::new(),
                message: error.to_string(),
            }],
        },
        other => {
            warn!(%registration, %network, "unexpected SDK error while resolving");
            FluentError::internal(other)
        }
    }
}

fn classify_trp(error: trp::Error, resolution: Resolution<'_>) -> FluentError {
    let Resolution {
        registration,
        network,
    } = resolution;
    let unavailable = |status: Option<u16>, kind: &str| {
        warn!(%network, status, kind, "resolver unavailable");
        FluentError::ResolverUnavailable {
            network: network.to_string(),
            status,
        }
    };
    let unusable = |reason: String| {
        warn!(%registration, %network, "the resolver cannot use the registration's template");
        FluentError::RegistrationUnavailable {
            registration: registration.to_string(),
            reason,
        }
    };

    match error {
        trp::Error::InputNotResolved(diagnostic) => {
            let input = Some(diagnostic.name.clone());
            let counts = Some(InputDiagnostic {
                has_address: diagnostic.query.address.is_some(),
                refs: diagnostic.query.refs.len(),
                matched: diagnostic.search_space.matched.len(),
            });
            debug!(%registration, %network, "input not resolved");
            if diagnostic.query.min_amount.is_empty() {
                FluentError::InputNotResolved {
                    input,
                    diagnostic: counts,
                }
            } else {
                FluentError::InsufficientFunds {
                    input,
                    diagnostic: counts,
                }
            }
        }
        trp::Error::TxScriptFailure(diagnostic) => {
            debug!(%registration, %network, "script failure");
            FluentError::ScriptFailure {
                logs: diagnostic.logs.into_iter().take(SCRIPT_LOG_LINES).collect(),
            }
        }
        trp::Error::MissingTxArg(diagnostic) => FluentError::InvalidArguments {
            reason: format!(
                "missing argument `{}` of type {}",
                diagnostic.key, diagnostic.arg_type
            ),
            violations: vec![Violation {
                path: super::arguments::pointer(&diagnostic.key),
                message: format!("missing argument of type {}", diagnostic.arg_type),
            }],
            arguments: vec![diagnostic.key],
        },
        trp::Error::UnsupportedTir(diagnostic) => unusable(format!(
            "the resolver does not support TIR version {}; it expects {}",
            diagnostic.provided, diagnostic.expected
        )),
        trp::Error::InvalidTirEnvelope
        | trp::Error::InvalidTirBytes
        | trp::Error::UnsupportedTxEra => unusable(format!(
            "the resolver rejected the transaction template: {error}"
        )),
        trp::Error::NetworkError(error) => {
            unavailable(error.status().map(|status| status.as_u16()), "network")
        }
        trp::Error::HttpError(status, _) => unavailable(Some(status), "http"),
        trp::Error::GenericRpcError(code, _, _) => {
            warn!(%network, rpc_code = code, "resolver returned an unclassified error");
            unavailable(None, "rpc")
        }
        trp::Error::DeserializationError(_) => unavailable(None, "malformed reply"),
        trp::Error::UnknownError(_) => unavailable(None, "empty reply"),
        trp::Error::UnsupportedEra { .. } => unavailable(None, "node era"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use tx3_sdk::trp::{
        InputNotResolvedDiagnostic, MissingTxArgDiagnostic, TxScriptFailureDiagnostic,
        UnsupportedTirDiagnostic,
    };

    use super::*;
    use crate::ErrorCode;

    const AT: Resolution<'static> = Resolution {
        registration: "transfer_preprod",
        network: "preprod",
    };

    fn trp(error: trp::Error) -> FluentError {
        classify(facade::Error::Trp(error), AT)
    }

    fn not_resolved(min_amount: HashMap<String, String>) -> trp::Error {
        let diagnostic: InputNotResolvedDiagnostic = serde_json::from_value(json!({
            "name": "source",
            "query": {
                "address": "addr_test1secret",
                "collateral": false,
                "minAmount": min_amount,
                "refs": ["aa#0", "bb#1"],
                "supportMany": true
            },
            "search_space": { "matched": ["cc#2"] }
        }))
        .unwrap();
        trp::Error::InputNotResolved(Box::new(diagnostic))
    }

    #[test]
    fn input_not_resolved_splits_on_min_amount() {
        let lovelace = HashMap::from([("lovelace".to_string(), "5000000".to_string())]);
        let funds = trp(not_resolved(lovelace));
        assert_eq!(funds.code(), ErrorCode::InsufficientFunds);
        let details = json!({
            "input": "source",
            "query": { "has_address": true, "refs": 2 },
            "search_space": { "matched": 1 }
        });
        assert_eq!(funds.details(), Some(details.clone()));

        let missing = trp(not_resolved(HashMap::new()));
        assert_eq!(missing.code(), ErrorCode::InputNotResolved);
        assert_eq!(missing.details(), Some(details));
        for err in [funds, missing] {
            let text = format!("{} {:?}", err.message(), err.details());
            assert!(!text.contains("addr_test1secret"), "{text}");
            assert!(!text.contains("5000000"), "{text}");
        }
    }

    #[test]
    fn script_failures_keep_the_first_log_lines() {
        let logs: Vec<String> = (0..25).map(|n| format!("trace {n}")).collect();
        let err = trp(trp::Error::TxScriptFailure(TxScriptFailureDiagnostic {
            logs,
        }));
        assert_eq!(err.code(), ErrorCode::ScriptFailure);
        let kept = err.details().unwrap()["logs"].as_array().unwrap().len();
        assert_eq!(kept, SCRIPT_LOG_LINES);
    }

    #[test]
    fn missing_arguments_are_invalid_arguments() {
        let err = trp(trp::Error::MissingTxArg(MissingTxArgDiagnostic {
            key: "quantity".into(),
            arg_type: "Int".into(),
        }));
        assert_eq!(err.code(), ErrorCode::InvalidArguments);
        assert_eq!(
            err.details(),
            Some(json!({
                "arguments": ["quantity"],
                "violations": [{ "path": "/quantity", "message": "missing argument of type Int" }]
            }))
        );
    }

    #[test]
    fn unusable_templates_are_registration_unavailable() {
        let unsupported = trp::Error::UnsupportedTir(UnsupportedTirDiagnostic {
            expected: "v1beta1".into(),
            provided: "v1beta0".into(),
        });
        for error in [
            unsupported,
            trp::Error::InvalidTirEnvelope,
            trp::Error::InvalidTirBytes,
        ] {
            let err = trp(error);
            assert_eq!(err.code(), ErrorCode::RegistrationUnavailable);
            assert_eq!(
                err.details(),
                Some(json!({ "registration": "transfer_preprod" }))
            );
        }
    }

    #[test]
    fn resolver_failures_carry_the_status_only() {
        let err = trp(trp::Error::HttpError(503, "Service Unavailable".into()));
        assert_eq!(err.code(), ErrorCode::ResolverUnavailable);
        assert_eq!(
            err.details(),
            Some(json!({ "network": "preprod", "status": 503 }))
        );
        for error in [
            trp::Error::GenericRpcError(-32600, "echo addr_test1secret".into(), None),
            trp::Error::DeserializationError("bad".into()),
            trp::Error::UnknownError("none".into()),
        ] {
            let err = trp(error);
            assert_eq!(err.code(), ErrorCode::ResolverUnavailable);
            assert_eq!(err.details(), Some(json!({ "network": "preprod" })));
        }
    }

    #[test]
    fn unknown_transactions_and_parties() {
        let err = classify(facade::Error::UnknownTx("swap".into()), AT);
        assert_eq!(err.code(), ErrorCode::UnknownTransaction);
        let err = classify(facade::Error::UnknownParty("ghost".into()), AT);
        assert_eq!(err.code(), ErrorCode::InvalidArguments);
        let err = classify(facade::Error::MissingTrpEndpoint, AT);
        assert_eq!(err.code(), ErrorCode::Internal);
    }
}
