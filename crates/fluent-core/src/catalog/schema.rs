//! Self-contained JSON Schemas from TII parameter schemas.
//!
//! A TII refers to the TII specification's `$defs` (`Address`, `Bytes`, …)
//! and to its own `components.schemas` by `$ref`. MCP clients cannot be
//! trusted to resolve either, so [`inline`] replaces every `$ref` with the
//! schema it names:
//!
//! - `https://tx3.land/specs/v1beta0/tii#/$defs/{Name}` and the legacy
//!   `https://tx3.land/specs/v1beta0/core#{Name}` resolve to `defs[Name]`;
//!   [`tii_defs`] is the table Fluent uses;
//! - `#/components/schemas/{Name}` resolves to `components[Name]`.
//!
//! Keywords written next to a `$ref` are kept and win over the referenced
//! schema's, so a parameter's own `description` survives. Every other keyword
//! is copied unchanged.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

/// Prefix of a `$ref` to the TII specification's `$defs`.
pub const TII_DEFS_PREFIX: &str = "https://tx3.land/specs/v1beta0/tii#/$defs/";

/// Prefix of a legacy `$ref` to the TII core types.
pub const LEGACY_CORE_PREFIX: &str = "https://tx3.land/specs/v1beta0/core#";

/// Prefix of a `$ref` to the TII's own `components.schemas`.
pub const COMPONENTS_PREFIX: &str = "#/components/schemas/";

/// Why a schema could not be made self-contained.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InlineError {
    /// A `$ref` names nothing in the ref table, or is not a string.
    #[error("unresolved $ref `{reference}` at `{path}`")]
    Unresolved {
        /// The `$ref` value.
        reference: String,
        /// Where the `$ref` is; see [`InlineError::path`].
        path: String,
    },
    /// A `$ref` refers, directly or through others, to itself.
    #[error("recursive $ref `{reference}` at `{path}` cannot be inlined")]
    Recursive {
        /// The `$ref` value.
        reference: String,
        /// Where the `$ref` is; see [`InlineError::path`].
        path: String,
    },
    /// A `$ref` resolves to something that is not a schema.
    #[error("$ref `{reference}` at `{path}` resolves to a value that is not a schema")]
    NotASchema {
        /// The `$ref` value.
        reference: String,
        /// Where the `$ref` is; see [`InlineError::path`].
        path: String,
    },
}

/// The TII specification `$defs` Fluent inlines, by name.
///
/// These follow `tii.json` (TII v1beta0) with two deliberate differences: an
/// `Address` is described as a bech32 address rather than carrying the
/// non-standard `format: bech32`, and a `UtxoRef` may omit the `0x` prefix, as
/// deployment profiles write them.
pub fn tii_defs() -> BTreeMap<String, Value> {
    let bytes = json!({
        "type": "string",
        "pattern": "^(0x)?[0-9a-fA-F]*$",
        "description": "Hex-encoded byte string, with an optional 0x prefix"
    });
    BTreeMap::from([
        (
            "Address".to_string(),
            json!({ "type": "string", "description": "bech32 address" }),
        ),
        ("Bytes".to_string(), bytes.clone()),
        (
            "UtxoRef".to_string(),
            json!({ "type": "string", "pattern": "^(0x)?[0-9a-fA-F]{64}#[0-9]+$" }),
        ),
        (
            "Utxo".to_string(),
            json!({ "type": "object", "description": "A resolved UTxO" }),
        ),
        (
            "AnyAsset".to_string(),
            json!({
                "type": "object",
                "description": "An asset identified at runtime by policy and name",
                "properties": {
                    "policy": bytes,
                    "asset_name": bytes,
                    "amount": { "type": "integer" }
                }
            }),
        ),
    ])
}

impl InlineError {
    /// Where the offending `$ref` is, as a URI fragment: `#/properties/x`
    /// inside the schema given to [`inline`], or the referenced location
    /// followed by a pointer, such as `#/components/schemas/Order/properties/x`,
    /// inside a schema reached through a `$ref`.
    pub fn path(&self) -> &str {
        match self {
            InlineError::Unresolved { path, .. }
            | InlineError::Recursive { path, .. }
            | InlineError::NotASchema { path, .. } => path,
        }
    }
}

/// Returns `schema` with every `$ref` replaced by the schema it names; see the
/// [module documentation](self) for the ref table.
///
/// Pure: it reads nothing but its arguments. Fails on a `$ref` it cannot
/// resolve and on a recursive one, which has no finite inline form.
pub fn inline(
    schema: &Value,
    components: &HashMap<String, Value>,
    defs: &BTreeMap<String, Value>,
) -> Result<Value, InlineError> {
    let inliner = Inliner { components, defs };
    inliner.schema(schema, &mut Vec::new(), &mut "#".to_string())
}

/// Keywords whose value is one subschema.
const SCHEMA_KEYWORDS: [&str; 12] = [
    "items",
    "additionalItems",
    "additionalProperties",
    "unevaluatedItems",
    "unevaluatedProperties",
    "contains",
    "propertyNames",
    "not",
    "if",
    "then",
    "else",
    "contentSchema",
];

/// Keywords whose value is an array of subschemas (`items` also appears here
/// in its pre-2020-12 array form).
const SCHEMA_ARRAY_KEYWORDS: [&str; 5] = ["prefixItems", "items", "allOf", "anyOf", "oneOf"];

/// Keywords whose value maps names to subschemas.
const SCHEMA_MAP_KEYWORDS: [&str; 5] = [
    "properties",
    "patternProperties",
    "dependentSchemas",
    "$defs",
    "definitions",
];

struct Inliner<'a> {
    components: &'a HashMap<String, Value>,
    defs: &'a BTreeMap<String, Value>,
}

impl Inliner<'_> {
    fn resolve(&self, reference: &str) -> Option<&Value> {
        if let Some(name) = reference.strip_prefix(COMPONENTS_PREFIX) {
            return self.components.get(name);
        }
        let name = reference
            .strip_prefix(TII_DEFS_PREFIX)
            .or_else(|| reference.strip_prefix(LEGACY_CORE_PREFIX))?;
        self.defs.get(name)
    }

    /// Inlines one schema. `stack` holds the `$ref`s being expanded, to catch
    /// recursion; `path` is where `schema` is (see [`InlineError::path`]).
    fn schema(
        &self,
        schema: &Value,
        stack: &mut Vec<String>,
        path: &mut String,
    ) -> Result<Value, InlineError> {
        let Value::Object(map) = schema else {
            // Boolean schemas, and anything else, carry no `$ref`.
            return Ok(schema.clone());
        };

        let mut out = match map.get("$ref") {
            Some(reference) => self.reference(reference, stack, path)?,
            None => Map::new(),
        };
        for (key, value) in map {
            if key == "$ref" {
                continue;
            }
            let len = path.len();
            push_segment(path, key);
            let keyword = key.as_str();
            let value = if SCHEMA_KEYWORDS.contains(&keyword) && !value.is_array() {
                self.schema(value, stack, path)?
            } else if (SCHEMA_ARRAY_KEYWORDS.contains(&keyword) && value.is_array())
                || (SCHEMA_MAP_KEYWORDS.contains(&keyword) && value.is_object())
            {
                self.each(value, stack, path)?
            } else {
                value.clone()
            };
            path.truncate(len);
            out.insert(key.clone(), value);
        }
        Ok(Value::Object(out))
    }

    /// Inlines every subschema in an array or a name → schema map.
    fn each(
        &self,
        value: &Value,
        stack: &mut Vec<String>,
        path: &mut String,
    ) -> Result<Value, InlineError> {
        let mut inline = |key: &str, schema: &Value| {
            let len = path.len();
            push_segment(path, key);
            let result = self.schema(schema, stack, path);
            path.truncate(len);
            result
        };
        Ok(match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| inline(&index.to_string(), item))
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, item)| Ok((key.clone(), inline(key, item)?)))
                    .collect::<Result<_, InlineError>>()?,
            ),
            other => other.clone(),
        })
    }

    /// The inlined keywords of the schema `reference` names.
    fn reference(
        &self,
        reference: &Value,
        stack: &mut Vec<String>,
        path: &str,
    ) -> Result<Map<String, Value>, InlineError> {
        let site = path.to_string();
        let Some(reference) = reference.as_str() else {
            return Err(InlineError::Unresolved {
                reference: reference.to_string(),
                path: site.clone(),
            });
        };
        if stack.iter().any(|seen| seen == reference) {
            return Err(InlineError::Recursive {
                reference: reference.to_string(),
                path: site.clone(),
            });
        }
        let target = self
            .resolve(reference)
            .ok_or_else(|| InlineError::Unresolved {
                reference: reference.to_string(),
                path: site.clone(),
            })?;

        // Inside the target, locations are relative to the reference.
        let mut inner = reference.to_string();
        stack.push(reference.to_string());
        let resolved = self.schema(target, stack, &mut inner);
        stack.pop();
        match resolved? {
            Value::Object(map) => Ok(map),
            Value::Bool(true) => Ok(Map::new()),
            Value::Bool(false) => Ok(Map::from_iter([("not".to_string(), json!({}))])),
            _ => Err(InlineError::NotASchema {
                reference: reference.to_string(),
                path: site,
            }),
        }
    }
}

/// Appends `/segment` to a JSON Pointer, escaping `~` and `/`.
fn push_segment(path: &mut String, segment: &str) {
    path.push('/');
    path.push_str(&segment.replace('~', "~0").replace('/', "~1"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline_with(schema: Value, components: Value) -> Result<Value, InlineError> {
        let components: HashMap<String, Value> = serde_json::from_value(components).unwrap();
        inline(&schema, &components, &tii_defs())
    }

    #[test]
    fn resolves_spec_and_legacy_core_refs() {
        let out = inline_with(
            json!({
                "type": "object",
                "properties": {
                    "a": { "$ref": "https://tx3.land/specs/v1beta0/tii#/$defs/Bytes" },
                    "b": { "$ref": "https://tx3.land/specs/v1beta0/core#Bytes" }
                }
            }),
            json!({}),
        )
        .unwrap();
        let bytes = &tii_defs()["Bytes"];
        assert_eq!(&out["properties"]["a"], bytes);
        assert_eq!(&out["properties"]["b"], bytes);
    }

    #[test]
    fn sibling_keywords_win_over_the_referenced_schema() {
        let out = inline_with(
            json!({
                "$ref": "https://tx3.land/specs/v1beta0/tii#/$defs/Bytes",
                "description": "The owner's key hash",
                "minLength": 56
            }),
            json!({}),
        )
        .unwrap();
        assert_eq!(
            out,
            json!({
                "type": "string",
                "pattern": "^(0x)?[0-9a-fA-F]*$",
                "description": "The owner's key hash",
                "minLength": 56
            })
        );
    }

    #[test]
    fn resolves_components_through_nested_refs() {
        let out = inline_with(
            json!({ "type": "array", "items": { "$ref": "#/components/schemas/Outer" } }),
            json!({
                "Outer": {
                    "type": "object",
                    "properties": { "inner": { "$ref": "#/components/schemas/Inner" } },
                    "required": ["inner"]
                },
                "Inner": { "oneOf": [{ "type": "integer", "minimum": 1 }, { "type": "null" }] }
            }),
        )
        .unwrap();
        assert_eq!(
            out,
            json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "inner": { "oneOf": [{ "type": "integer", "minimum": 1 }, { "type": "null" }] }
                    },
                    "required": ["inner"]
                }
            })
        );
    }

    #[test]
    fn a_component_used_twice_is_not_recursion() {
        let out = inline_with(
            json!({
                "type": "array",
                "prefixItems": [
                    { "$ref": "#/components/schemas/Unit" },
                    { "$ref": "#/components/schemas/Unit" }
                ]
            }),
            json!({ "Unit": { "type": "null" } }),
        )
        .unwrap();
        assert_eq!(
            out["prefixItems"],
            json!([{ "type": "null" }, { "type": "null" }])
        );
    }

    #[test]
    fn data_keywords_are_copied_unchanged() {
        let schema = json!({
            "enum": [{ "$ref": "not a reference" }],
            "default": { "$ref": "neither" },
            "required": ["$ref"]
        });
        assert_eq!(inline_with(schema.clone(), json!({})).unwrap(), schema);
    }

    #[test]
    fn boolean_components_become_object_schemas() {
        let out = inline_with(
            json!({
                "properties": {
                    "any": { "$ref": "#/components/schemas/Any" },
                    "none": { "$ref": "#/components/schemas/None" }
                }
            }),
            json!({ "Any": true, "None": false }),
        )
        .unwrap();
        assert_eq!(out["properties"]["any"], json!({}));
        assert_eq!(out["properties"]["none"], json!({ "not": {} }));
    }

    #[test]
    fn unknown_refs_are_errors_naming_the_site() {
        for reference in [
            "#/components/schemas/Missing",
            "https://tx3.land/specs/v1beta0/tii#/$defs/TirEnvelope",
            "#/$defs/Bytes",
            "https://example.com/schema",
        ] {
            let err = inline_with(
                json!({ "properties": { "a/b": { "$ref": reference } } }),
                json!({}),
            )
            .unwrap_err();
            assert_eq!(
                err,
                InlineError::Unresolved {
                    reference: reference.to_string(),
                    path: "#/properties/a~1b".to_string()
                }
            );
        }
    }

    #[test]
    fn recursive_components_are_errors() {
        let err = inline_with(
            json!({ "$ref": "#/components/schemas/List" }),
            json!({
                "List": {
                    "oneOf": [
                        { "type": "null" },
                        { "type": "array", "prefixItems": [{ "type": "integer" }, { "$ref": "#/components/schemas/List" }] }
                    ]
                }
            }),
        )
        .unwrap_err();
        assert_eq!(
            err,
            InlineError::Recursive {
                reference: "#/components/schemas/List".to_string(),
                path: "#/components/schemas/List/oneOf/1/prefixItems/1".to_string()
            }
        );
    }

    #[test]
    fn non_schema_targets_are_errors() {
        let err = inline_with(
            json!({ "$ref": "#/components/schemas/Text" }),
            json!({ "Text": "hello" }),
        )
        .unwrap_err();
        assert!(matches!(err, InlineError::NotASchema { .. }), "{err:?}");
    }
}
