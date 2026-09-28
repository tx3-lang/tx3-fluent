//! Checking a caller's arguments before anything is resolved.
//!
//! [`ArgumentRules`] holds, for one transaction under one deployment profile,
//! the compiled input schema of its tool (see [`catalog::input_schema`]) and
//! the names the profile binds. [`ArgumentRules::check`] rejects, in order:
//!
//! 1. any key naming a party or environment field the profile binds,
//!    compared in lowercase, with "deployment-bound value cannot be
//!    overridden";
//! 2. anything the schema rejects: missing required arguments, unknown keys,
//!    wrong types, broken patterns and every other keyword.
//!
//! Violations name the offending location and never quote the value.

use std::collections::BTreeSet;

use jsonschema::error::ValidationErrorKind;
use jsonschema::{ValidationError, Validator};
use serde_json::{Map, Value};
use tx3_sdk::tii::spec::TiiFile;

use crate::catalog;
use crate::error::{FluentError, Violation};

/// The message of a violation that tries to override a bound value.
pub const BOUND_OVERRIDE: &str = "deployment-bound value cannot be overridden";

/// Placeholder that stands for the caller's value in violation messages.
const VALUE_PLACEHOLDER: &str = "value";

/// How to check the arguments of one transaction under one deployment
/// profile.
#[derive(Debug)]
pub struct ArgumentRules {
    validator: Validator,
    /// Lowercased names of the parties and environment fields the profile
    /// binds.
    bound: BTreeSet<String>,
    /// Names of the parties the caller supplies, as the input schema spells
    /// them (lowercase).
    parties: BTreeSet<String>,
}

/// Arguments that passed [`ArgumentRules::check`], split for the SDK.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedArguments {
    /// Each caller-supplied party and its address, in name order.
    pub parties: Vec<(String, String)>,
    /// Every other argument: transaction parameters and unbound environment
    /// fields.
    pub args: Map<String, Value>,
}

impl ArgumentRules {
    /// The rules for transaction `tx` of `tii` under `profile`.
    ///
    /// Fails, with a reason naming `tx`, when the input schema cannot be
    /// built or compiled.
    pub fn new(tii: &TiiFile, tx: &str, profile: &str) -> Result<ArgumentRules, String> {
        let schema = catalog::input_schema(tii, tx, profile)
            .map_err(|reason| format!("transaction `{tx}`: {reason}"))?;
        let validator = jsonschema::validator_for(&schema)
            .map_err(|err| format!("transaction `{tx}`: input schema does not compile: {err}"))?;

        let values = tii
            .profiles
            .get(profile)
            .ok_or_else(|| format!("the TII defines no profile `{profile}`"))?;
        let mut bound: BTreeSet<String> = values.parties.keys().map(|p| p.to_lowercase()).collect();
        if let Some(environment) = values.environment.as_object() {
            bound.extend(environment.keys().map(|key| key.to_lowercase()));
        }
        let parties = tii
            .parties
            .keys()
            .map(|party| party.to_lowercase())
            .filter(|party| !bound.contains(party))
            .collect();

        Ok(ArgumentRules {
            validator,
            bound,
            parties,
        })
    }

    /// Checks `args` and splits them into parties and other arguments.
    ///
    /// Fails with [`FluentError::InvalidArguments`] listing every violation of
    /// the first failing step: bound overrides first, then the schema.
    pub fn check(&self, args: &Value) -> Result<CheckedArguments, FluentError> {
        if let Some(object) = args.as_object() {
            let overrides: Vec<&String> = object
                .keys()
                .filter(|key| self.bound.contains(&key.to_lowercase()))
                .collect();
            if !overrides.is_empty() {
                return Err(FluentError::InvalidArguments {
                    reason: BOUND_OVERRIDE.to_string(),
                    arguments: overrides.iter().map(|key| key.to_string()).collect(),
                    violations: overrides
                        .iter()
                        .map(|key| Violation {
                            path: pointer(key),
                            message: BOUND_OVERRIDE.to_string(),
                        })
                        .collect(),
                });
            }
        }

        let mut arguments = BTreeSet::new();
        let mut violations = Vec::new();
        for error in self.validator.iter_errors(args) {
            arguments.extend(argument_names(&error));
            violations.push(Violation {
                path: error.instance_path().as_str().to_string(),
                message: masked_message(&error),
            });
        }
        if let Some(first) = violations.first() {
            let mut reason = match first.path.as_str() {
                "" => first.message.clone(),
                path => format!("{} at {path}", first.message),
            };
            if violations.len() > 1 {
                reason.push_str(&format!(" ({} more)", violations.len() - 1));
            }
            return Err(FluentError::InvalidArguments {
                reason,
                arguments: arguments.into_iter().collect(),
                violations,
            });
        }

        let mut checked = CheckedArguments {
            parties: Vec::new(),
            args: Map::new(),
        };
        for (key, value) in args.as_object().into_iter().flatten() {
            match value {
                Value::String(address) if self.parties.contains(key) => {
                    checked.parties.push((key.clone(), address.clone()));
                }
                _ => {
                    checked.args.insert(key.clone(), value.clone());
                }
            }
        }
        checked.parties.sort();
        Ok(checked)
    }
}

/// `key` as a one-segment JSON Pointer.
pub(crate) fn pointer(key: &str) -> String {
    format!("/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// The error's message with the caller's value replaced by a placeholder.
fn masked_message(error: &ValidationError<'_>) -> String {
    match error.kind() {
        // A masked `propertyNames` error still shows its inner error
        // unmasked, so mask the inner error itself.
        ValidationErrorKind::PropertyNames { error: inner } => {
            inner.masked_with(VALUE_PLACEHOLDER).to_string()
        }
        _ => error.masked_with(VALUE_PLACEHOLDER).to_string(),
    }
}

/// The top-level argument names an error concerns.
fn argument_names(error: &ValidationError<'_>) -> Vec<String> {
    if let Some(first) = error.instance_path().segments().next() {
        return vec![first.to_string()];
    }
    match error.kind() {
        ValidationErrorKind::Required { property } => {
            property.as_str().map(str::to_string).into_iter().collect()
        }
        ValidationErrorKind::AdditionalProperties { unexpected }
        | ValidationErrorKind::UnevaluatedProperties { unexpected } => unexpected.clone(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A TII whose `pay` transaction takes a pattern-checked `reference`, an
    /// optional `memo` and a `Payer` party; the preprod profile binds the
    /// `Payee` party and the `fee` environment field.
    fn tii() -> TiiFile {
        serde_json::from_value(json!({
            "tii": { "version": "v1beta0" },
            "protocol": { "scope": "acme", "name": "pay", "version": "1.0.0" },
            "environment": {
                "type": "object",
                "properties": { "fee": { "type": "integer" }, "note": { "type": "string" } },
                "required": ["fee"]
            },
            "parties": { "Payer": {}, "Payee": {} },
            "transactions": {
                "pay": {
                    "tir": { "content": "", "encoding": "hex", "version": "v1beta0" },
                    "params": {
                        "type": "object",
                        "properties": {
                            "reference": {
                                "$ref": "https://tx3.land/specs/v1beta0/tii#/$defs/UtxoRef"
                            },
                            "amount": { "type": "integer", "minimum": 1 },
                            "memo": { "type": "string" }
                        },
                        "required": ["reference", "amount"]
                    }
                }
            },
            "profiles": {
                "preprod": {
                    "environment": { "fee": 5 },
                    "parties": { "Payee": "addr_test1bound" }
                }
            }
        }))
        .unwrap()
    }

    fn rules() -> ArgumentRules {
        ArgumentRules::new(&tii(), "pay", "preprod").unwrap()
    }

    const REFERENCE: &str = "4f3cbd2b6b9b2c1e0a3b3a0a1f6a9f8a2c1d3e4f5a6b7c8d9e0f1a2b3c4d5e6f#1";

    fn valid() -> Value {
        json!({
            "reference": REFERENCE,
            "amount": 7_777_777,
            "payer": "addr_test1payer",
            "note": "thanks"
        })
    }

    fn invalid(args: Value) -> (Vec<String>, Vec<Violation>, String) {
        match rules().check(&args).unwrap_err() {
            FluentError::InvalidArguments {
                reason,
                arguments,
                violations,
            } => (arguments, violations, reason),
            other => panic!("expected invalid_arguments, got {other:?}"),
        }
    }

    #[test]
    fn valid_arguments_are_split_into_parties_and_args() {
        let checked = rules().check(&valid()).unwrap();
        assert_eq!(
            checked.parties,
            [("payer".to_string(), "addr_test1payer".to_string())]
        );
        assert_eq!(
            Value::Object(checked.args),
            json!({ "reference": REFERENCE, "amount": 7_777_777, "note": "thanks" })
        );
    }

    #[test]
    fn missing_required_arguments_are_rejected() {
        let mut args = valid();
        args.as_object_mut().unwrap().remove("reference");
        args.as_object_mut().unwrap().remove("payer");
        let (arguments, violations, reason) = invalid(args);
        assert_eq!(arguments, ["payer", "reference"]);
        let messages: Vec<&str> = violations.iter().map(|v| v.message.as_str()).collect();
        assert!(
            messages.contains(&"\"reference\" is a required property"),
            "{messages:?}"
        );
        assert!(
            messages.contains(&"\"payer\" is a required property"),
            "{messages:?}"
        );
        assert!(
            violations.iter().all(|v| v.path.is_empty()),
            "{violations:?}"
        );
        assert!(reason.ends_with("(1 more)"), "{reason}");
    }

    #[test]
    fn extra_keys_are_rejected() {
        let mut args = valid();
        args["surprise"] = json!("extra-value-5150");
        let (arguments, violations, reason) = invalid(args);
        assert_eq!(arguments, ["surprise"]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "");
        assert!(
            violations[0].message.contains("'surprise'"),
            "{violations:?}"
        );
        assert!(!reason.contains("extra-value-5150"), "{reason}");
    }

    #[test]
    fn bound_values_cannot_be_overridden() {
        for key in ["fee", "Fee", "payee", "PAYEE"] {
            let mut args = valid();
            args[key] = json!("override-attempt-4242");
            let (arguments, violations, reason) = invalid(args);
            assert_eq!(reason, BOUND_OVERRIDE, "{key}");
            assert_eq!(arguments, [key]);
            assert_eq!(
                violations,
                [Violation {
                    path: format!("/{key}"),
                    message: BOUND_OVERRIDE.to_string()
                }]
            );
        }
    }

    #[test]
    fn wrong_patterns_are_rejected_without_quoting_the_value() {
        let mut args = valid();
        args["reference"] = json!("not-a-utxo-ref-8086");
        let (arguments, violations, reason) = invalid(args);
        assert_eq!(arguments, ["reference"]);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].path, "/reference");
        assert!(
            violations[0].message.starts_with("value does not match"),
            "{violations:?}"
        );
        assert!(reason.ends_with("at /reference"), "{reason}");
        for text in [&violations[0].message, &reason] {
            assert!(!text.contains("8086"), "leaked the value: {text}");
        }
    }

    #[test]
    fn wrong_types_and_ranges_are_rejected_without_quoting_the_value() {
        let mut args = valid();
        args["amount"] = json!(-31337);
        args["memo"] = json!(90210);
        let (arguments, violations, _) = invalid(args);
        assert_eq!(arguments, ["amount", "memo"]);
        for violation in &violations {
            assert!(!violation.message.contains("31337"), "{violation:?}");
            assert!(!violation.message.contains("90210"), "{violation:?}");
        }
    }

    #[test]
    fn arguments_must_be_an_object() {
        let (arguments, violations, reason) = invalid(json!(["payer"]));
        assert!(arguments.is_empty());
        assert_eq!(violations[0].path, "");
        assert_eq!(reason, "value is not of type \"object\"");
    }

    #[test]
    fn keys_are_escaped_in_pointers() {
        assert_eq!(pointer("a/b~c"), "/a~1b~0c");
    }
}
