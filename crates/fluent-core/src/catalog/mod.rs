//! The tool catalog: one MCP tool descriptor per transaction of every loaded
//! registration, plus the fixed tools every server offers.
//!
//! [`build_tools`] turns a [`Registration`] into [`ToolDescriptor`]s, one per
//! TII transaction, in transaction name order:
//!
//! - **name** `{slug}_{tx}`, where `tx` is the transaction name lowercased
//!   with every character outside `[a-z0-9_]` replaced by `_`; at most
//!   [`MAX_TOOL_NAME_LEN`] characters;
//! - **title** `{protocol name} {version} · {tx} ({network})`, with the
//!   transaction name as the TII writes it;
//! - **description** the TII transaction description, when there is one,
//!   then a fixed trailer saying the transaction is prepared unsigned;
//! - **input schema** see [`input_schema`];
//! - **output schema** the [`PreparedTransaction`] schema;
//! - **annotations** read-only, not destructive, not idempotent, open world.
//!
//! [`fixed_tools`] describes `fluent_get_skill` and `fluent_inspect_address`.
//! [`all_tools`] returns both for a whole [`Catalog`] and rejects duplicate
//! names, which would make a tool unreachable.

pub mod schema;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tx3_sdk::tii::spec::TiiFile;

use crate::envelope::PreparedTransaction;
use crate::error::FluentError;
use crate::registration::{Catalog, Network, Registration, SkillDependency};

/// The longest tool name a descriptor may have.
pub const MAX_TOOL_NAME_LEN: usize = 64;

/// Name of the fixed tool that returns a protocol's consumption skill.
pub const GET_SKILL_TOOL: &str = "fluent_get_skill";

/// Name of the fixed tool that reports what a Cardano address is.
pub const INSPECT_ADDRESS_TOOL: &str = "fluent_inspect_address";

/// Name of the schema `fluent_inspect_address` returns. The type is defined by
/// the address inspection module; until it is part of this crate, the tool's
/// output schema is an object titled with this name.
pub const ADDRESS_REPORT_SCHEMA: &str = "AddressReport";

/// An MCP tool, described for listing. Transports turn it into their own tool
/// type; nothing here invokes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolDescriptor {
    /// Unique tool name.
    pub name: String,
    /// Human-readable title.
    pub title: String,
    /// What the tool does, for the model choosing it.
    pub description: String,
    /// JSON Schema of the arguments: a self-contained object schema, with no
    /// `$ref`.
    pub input_schema: Value,
    /// JSON Schema of the structured result.
    pub output_schema: Value,
    /// Behaviour hints.
    pub annotations: ToolAnnotations,
    /// The registration the tool prepares transactions for; `None` for a
    /// fixed tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_slug: Option<String>,
    /// The TII transaction the tool prepares, as the TII names it; `None` for
    /// a fixed tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_name: Option<String>,
}

/// MCP tool annotations: hints about a tool's behaviour, serialized with the
/// MCP field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// The tool changes nothing in its environment.
    pub read_only_hint: bool,
    /// The tool may destroy or overwrite data.
    pub destructive_hint: bool,
    /// Calling the tool again with the same arguments has no further effect
    /// and gives the same result.
    pub idempotent_hint: bool,
    /// The tool interacts with an open world of external entities.
    pub open_world_hint: bool,
}

impl ToolAnnotations {
    /// Transaction tools: preparing changes nothing, but it reads the chain,
    /// whose state moves, so the same call can prepare a different
    /// transaction.
    pub const TRANSACTION: ToolAnnotations = ToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: false,
        open_world_hint: true,
    };

    /// Fixed tools: they read only what the server loaded at startup, or
    /// their arguments.
    pub const FIXED: ToolAnnotations = ToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: false,
    };
}

/// The result of `fluent_get_skill`: a registration's consumption skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillResult {
    /// The registration the skill is bound to.
    pub protocol: SkillProtocol,
    /// The skill's own revision.
    pub skill_revision: u32,
    /// What the assistant must obtain outside Fluent, and for which transactions.
    pub dependencies: Vec<SkillDependency>,
    /// The skill's Markdown body.
    pub markdown: String,
}

/// The registration a [`SkillResult`] is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillProtocol {
    /// The registry scope that publishes the protocol.
    pub scope: String,
    /// The protocol name within its scope.
    pub name: String,
    /// The protocol version.
    pub version: String,
    /// The slug of the registration.
    pub registration_slug: String,
    /// The registration revision.
    pub registration_revision: String,
    /// The network the registration serves.
    pub network: Network,
}

/// One tool per transaction of `registration`, in transaction name order.
///
/// Fails with [`FluentError::RegistrationUnavailable`] when a tool name would
/// be empty, longer than [`MAX_TOOL_NAME_LEN`] or shared by two transactions,
/// or when a transaction's input schema cannot be built (see
/// [`input_schema`]).
pub fn build_tools(registration: &Registration) -> Result<Vec<ToolDescriptor>, FluentError> {
    transaction_tools(
        registration.slug(),
        registration.tii(),
        registration.profile(),
        registration.network(),
    )
    .map_err(|reason| FluentError::RegistrationUnavailable {
        registration: registration.bundle().display().to_string(),
        reason,
    })
}

/// [`build_tools`] over a registration's parts; the TII `protocol` block
/// equals the registration's.
fn transaction_tools(
    slug: &str,
    tii: &TiiFile,
    profile: &str,
    network: Network,
) -> Result<Vec<ToolDescriptor>, String> {
    let protocol = &tii.protocol;
    let output_schema = schema_of::<PreparedTransaction>();

    let mut transactions: Vec<_> = tii.transactions.iter().collect();
    transactions.sort_by(|a, b| a.0.cmp(b.0));

    let mut claimed: BTreeMap<String, &str> = BTreeMap::new();
    let mut tools = Vec::with_capacity(transactions.len());
    for (tx_name, transaction) in transactions {
        let name = tool_name(slug, tx_name)?;
        if let Some(other) = claimed.insert(name.clone(), tx_name) {
            return Err(format!(
                "transactions `{other}` and `{tx_name}` both map to tool name `{name}`"
            ));
        }
        let input_schema = input_schema(tii, tx_name, profile)
            .map_err(|reason| format!("transaction `{tx_name}`: {reason}"))?;

        let trailer = format!(
            "Prepares an UNSIGNED {network} transaction for {}/{}:{}. Nothing is signed or \
             submitted. Call {GET_SKILL_TOOL} for this protocol before using this tool.",
            protocol.scope, protocol.name, protocol.version
        );
        let description = match transaction.description.as_deref().map(str::trim) {
            Some(text) if !text.is_empty() => format!("{text}\n\n{trailer}"),
            _ => trailer,
        };

        tools.push(ToolDescriptor {
            name,
            title: format!(
                "{} {} · {tx_name} ({network})",
                protocol.name, protocol.version
            ),
            description,
            input_schema,
            output_schema: output_schema.clone(),
            annotations: ToolAnnotations::TRANSACTION,
            registration_slug: Some(slug.to_string()),
            tx_name: Some(tx_name.clone()),
        });
    }
    Ok(tools)
}

/// The tool name for transaction `tx` of registration `slug`: `{slug}_{tx}`,
/// with `tx` lowercased and every character outside `[a-z0-9_]` replaced by
/// `_`.
///
/// Fails when `tx` is empty or the name is longer than [`MAX_TOOL_NAME_LEN`].
pub fn tool_name(slug: &str, tx: &str) -> Result<String, String> {
    if tx.is_empty() {
        return Err("a transaction has an empty name".to_string());
    }
    let tx: String = tx
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = format!("{slug}_{tx}");
    let len = name.chars().count();
    if len > MAX_TOOL_NAME_LEN {
        return Err(format!(
            "tool name `{name}` is {len} characters long, more than {MAX_TOOL_NAME_LEN}"
        ));
    }
    Ok(name)
}

/// The input schema of transaction `tx` under deployment profile `profile`.
///
/// It starts from the transaction's `params` schema with every `$ref` inlined
/// ([`schema::inline`] with [`schema::tii_defs`] and the TII components),
/// then adds, after the parameters:
///
/// - one required `string` property per party the profile does not bind,
///   named as the party in lowercase and described as its bech32 address;
/// - every environment field the profile does not bind, with its TII schema,
///   required when the TII environment requires it.
///
/// Bound parties and environment fields are left out entirely, and
/// `additionalProperties` is `false`. Every keyword of the TII schemas
/// (`description`, `minimum`, `pattern`, `enum`, …) and the order of
/// `required` are kept.
///
/// Arguments are matched case-insensitively when a transaction is prepared,
/// so a parameter, party or environment field whose lowercased name equals
/// another's is an error, bound or not.
pub fn input_schema(tii: &TiiFile, tx: &str, profile: &str) -> Result<Value, String> {
    let transaction = tii
        .transactions
        .get(tx)
        .ok_or_else(|| format!("the TII defines no transaction `{tx}`"))?;
    let profile_values = tii
        .profiles
        .get(profile)
        .ok_or_else(|| format!("the TII defines no profile `{profile}`"))?;
    let no_components = HashMap::new();
    let components = tii
        .components
        .as_ref()
        .map_or(&no_components, |components| &components.schemas);
    let defs = schema::tii_defs();

    let params = schema::inline(&transaction.params, components, &defs)
        .map_err(|err| format!("params: {err}"))?;
    let ObjectSchema {
        mut root,
        mut properties,
        mut required,
    } = ObjectSchema::split(params, "params")?;
    match root.get("type") {
        None => {
            root.insert("type".to_string(), json!("object"));
        }
        Some(kind) if kind == "object" => {}
        Some(kind) => return Err(format!("params must be an object schema, not {kind}")),
    }

    let mut names = ArgumentNames::default();
    for name in properties.keys() {
        names.claim(name, "parameter")?;
    }

    let bound_parties: BTreeSet<String> = profile_values
        .parties
        .keys()
        .map(|party| party.to_lowercase())
        .collect();
    let mut parties: Vec<_> = tii.parties.iter().collect();
    parties.sort_by(|a, b| a.0.cmp(b.0));
    for (party, definition) in parties {
        let key = party.to_lowercase();
        names.claim(&key, "party")?;
        if bound_parties.contains(&key) {
            continue;
        }
        let mut description = format!("bech32 address of the {party} party");
        if let Some(extra) = definition.description.as_deref().map(str::trim)
            && !extra.is_empty()
        {
            description = format!("{description}. {extra}");
        }
        properties.insert(
            key.clone(),
            json!({ "type": "string", "description": description }),
        );
        required.push(Value::String(key));
    }

    if let Some(environment) = &tii.environment {
        let environment = schema::inline(environment, components, &defs)
            .map_err(|err| format!("environment: {err}"))?;
        let ObjectSchema {
            properties: fields,
            required: env_required,
            ..
        } = ObjectSchema::split(environment, "environment")?;
        let bound: BTreeSet<String> = profile_values
            .environment
            .as_object()
            .map(|values| values.keys().map(|key| key.to_lowercase()).collect())
            .unwrap_or_default();
        let is_bound = |name: &str| bound.contains(&name.to_lowercase());
        for name in fields.keys() {
            names.claim(name, "environment field")?;
        }
        for name in env_required.iter().filter_map(Value::as_str) {
            if fields.contains_key(name) && !is_bound(name) {
                required.push(json!(name));
            }
        }
        for (name, field) in fields {
            if !is_bound(&name) {
                properties.insert(name, field);
            }
        }
    }

    root.insert("properties".to_string(), Value::Object(properties));
    root.insert("required".to_string(), Value::Array(required));
    root.insert("additionalProperties".to_string(), json!(false));
    Ok(Value::Object(root))
}

/// An object schema taken apart: `properties`, `required` and every other
/// keyword.
struct ObjectSchema {
    root: Map<String, Value>,
    properties: Map<String, Value>,
    required: Vec<Value>,
}

impl ObjectSchema {
    /// Splits `schema`, checking the shapes of `properties` and `required`;
    /// `what` names it in errors.
    fn split(schema: Value, what: &str) -> Result<ObjectSchema, String> {
        let Value::Object(mut root) = schema else {
            return Err(format!("{what} must be a JSON object schema"));
        };
        let properties = match root.remove("properties") {
            None => Map::new(),
            Some(Value::Object(properties)) => properties,
            Some(_) => return Err(format!("{what} `properties` must be an object")),
        };
        let required = match root.remove("required") {
            None => Vec::new(),
            Some(Value::Array(names)) if names.iter().all(Value::is_string) => names,
            Some(_) => return Err(format!("{what} `required` must be an array of names")),
        };
        Ok(ObjectSchema {
            root,
            properties,
            required,
        })
    }
}

/// Argument names claimed so far, lowercased, with what claimed them.
#[derive(Default)]
struct ArgumentNames(BTreeMap<String, (String, &'static str)>);

impl ArgumentNames {
    fn claim(&mut self, name: &str, kind: &'static str) -> Result<(), String> {
        match self.0.get(&name.to_lowercase()) {
            Some((other, other_kind)) => Err(format!(
                "{kind} `{name}` has the same argument name as {other_kind} `{other}`"
            )),
            None => {
                self.0.insert(name.to_lowercase(), (name.to_string(), kind));
                Ok(())
            }
        }
    }
}

/// The two tools every server offers, whatever it has loaded:
/// `fluent_get_skill` and `fluent_inspect_address`.
pub fn fixed_tools() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: GET_SKILL_TOOL.to_string(),
            title: "Get protocol skill".to_string(),
            description: "Returns the consumption skill of a registered protocol: how to use \
                its transactions, what to obtain elsewhere first, and the registration and \
                network it applies to. Call it before any transaction tool of that protocol."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "protocol": {
                        "type": "string",
                        "description": "A registration slug, such as \
                            strike_staking_mainnet, or a protocol as scope/name, such as \
                            strike-finance/strike-staking."
                    }
                },
                "required": ["protocol"],
                "additionalProperties": false
            }),
            output_schema: schema_of::<SkillResult>(),
            annotations: ToolAnnotations::FIXED,
            registration_slug: None,
            tx_name: None,
        },
        ToolDescriptor {
            name: INSPECT_ADDRESS_TOOL.to_string(),
            title: "Inspect Cardano address".to_string(),
            description: "Reports the network, kind and credentials of a Cardano address, for \
                example to derive a key hash argument or to check that an address belongs to \
                the expected network. It makes no chain queries."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "address": {
                        "type": "string",
                        "description": "The address to inspect: bech32 (addr1…, \
                            addr_test1…, stake1…), hex, or base58 for Byron."
                    }
                },
                "required": ["address"],
                "additionalProperties": false
            }),
            output_schema: json!({ "type": "object", "title": ADDRESS_REPORT_SCHEMA }),
            annotations: ToolAnnotations::FIXED,
            registration_slug: None,
            tx_name: None,
        },
    ]
}

/// Every tool a server with `catalog` offers: the [`fixed_tools`], then each
/// registration's [`build_tools`], in slug order.
///
/// Fails on the first registration whose tools cannot be built, and when two
/// tools share a name.
pub fn all_tools(catalog: &Catalog) -> Result<Vec<ToolDescriptor>, FluentError> {
    let mut tools = fixed_tools();
    for registration in catalog.iter() {
        tools.extend(build_tools(registration)?);
    }
    check_unique(&tools)?;
    Ok(tools)
}

/// Fails when two tools share a name, naming whatever claimed it.
fn check_unique(tools: &[ToolDescriptor]) -> Result<(), FluentError> {
    let owner = |tool: &ToolDescriptor| match &tool.registration_slug {
        Some(slug) => format!("registration `{slug}`"),
        None => "a fixed tool".to_string(),
    };
    let mut seen: BTreeMap<&str, &ToolDescriptor> = BTreeMap::new();
    for tool in tools {
        if let Some(other) = seen.insert(&tool.name, tool) {
            return Err(FluentError::RegistrationUnavailable {
                registration: tool.registration_slug.clone().unwrap_or_default(),
                reason: format!(
                    "tool name `{}` is claimed by both {} and {}",
                    tool.name,
                    owner(other),
                    owner(tool)
                ),
            });
        }
    }
    Ok(())
}

/// The JSON Schema of `T` with every subschema inlined, so it has no `$ref`.
fn schema_of<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|settings| settings.inline_subschemas = true)
        .into_generator();
    generator.into_root_schema_for::<T>().to_value()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tii(value: Value) -> TiiFile {
        serde_json::from_value(value).unwrap()
    }

    fn minimal_tii(transactions: Value) -> TiiFile {
        tii(json!({
            "tii": { "version": "v1beta0" },
            "protocol": { "scope": "acme", "name": "swap", "version": "1.0.0" },
            "transactions": transactions,
            "profiles": { "preprod": { "environment": {}, "parties": {} } }
        }))
    }

    fn tx(params: Value) -> Value {
        json!({ "tir": { "content": "", "encoding": "hex", "version": "v1beta0" }, "params": params })
    }

    #[test]
    fn descriptions_lead_with_the_tii_description() {
        let mut file = minimal_tii(json!({
            "swap": tx(json!({})),
            "Quote": tx(json!({}))
        }));
        file.transactions.get_mut("swap").unwrap().description =
            Some("  Swaps one asset for another.\n".to_string());
        let tools = transaction_tools("acme_swap", &file, "preprod", Network::Preprod).unwrap();

        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["acme_swap_quote", "acme_swap_swap"]);
        assert_eq!(tools[0].title, "swap 1.0.0 · Quote (preprod)");
        assert_eq!(tools[0].tx_name.as_deref(), Some("Quote"));

        let trailer = "Prepares an UNSIGNED preprod transaction for acme/swap:1.0.0. Nothing is \
                       signed or submitted. Call fluent_get_skill for this protocol before using \
                       this tool.";
        assert_eq!(tools[0].description, trailer);
        assert_eq!(
            tools[1].description,
            format!("Swaps one asset for another.\n\n{trailer}")
        );
    }

    #[test]
    fn transactions_sharing_a_tool_name_are_errors() {
        let file = minimal_tii(json!({ "add-stake": tx(json!({})), "add_stake": tx(json!({})) }));
        let err = transaction_tools("acme_swap", &file, "preprod", Network::Preprod).unwrap_err();
        assert_eq!(
            err,
            "transactions `add-stake` and `add_stake` both map to tool name `acme_swap_add_stake`"
        );

        let file = minimal_tii(json!({ "t".repeat(30): tx(json!({})) }));
        let err =
            transaction_tools(&"s".repeat(40), &file, "preprod", Network::Preprod).unwrap_err();
        assert!(err.contains("more than 64"), "{err}");
    }

    #[test]
    fn tool_names_are_sanitized_and_bounded() {
        assert_eq!(tool_name("acme_swap", "swap").unwrap(), "acme_swap_swap");
        assert_eq!(
            tool_name("acme_swap", "Add-Liquidity.v2").unwrap(),
            "acme_swap_add_liquidity_v2"
        );
        assert_eq!(tool_name("abc", "Añadir").unwrap(), "abc_a_adir");

        let slug = "a".repeat(41);
        assert_eq!(tool_name(&slug, &"t".repeat(22)).unwrap().len(), 64);
        let err = tool_name(&slug, &"t".repeat(23)).unwrap_err();
        assert!(err.contains("65 characters"), "{err}");
        assert!(tool_name("abc", "").is_err());
    }

    #[test]
    fn transactions_without_params_take_no_arguments() {
        let schema = input_schema(
            &minimal_tii(json!({ "noop": tx(json!({})) })),
            "noop",
            "preprod",
        )
        .unwrap();
        assert_eq!(
            schema,
            json!({ "type": "object", "properties": {}, "required": [], "additionalProperties": false })
        );
    }

    #[test]
    fn party_descriptions_are_kept() {
        let mut file = minimal_tii(json!({ "pay": tx(json!({ "type": "object" })) }));
        file.parties = serde_json::from_value(json!({
            "Payee": { "description": "Who receives the payment." }
        }))
        .unwrap();
        let schema = input_schema(&file, "pay", "preprod").unwrap();
        assert_eq!(
            schema["properties"]["payee"],
            json!({
                "type": "string",
                "description": "bech32 address of the Payee party. Who receives the payment."
            })
        );
        assert_eq!(schema["required"], json!(["payee"]));
    }

    #[test]
    fn argument_name_clashes_are_errors() {
        let mut file = minimal_tii(json!({
            "pay": tx(json!({ "type": "object", "properties": { "Payee": { "type": "integer" } } }))
        }));
        file.parties = serde_json::from_value(json!({ "payee": {} })).unwrap();
        let err = input_schema(&file, "pay", "preprod").unwrap_err();
        assert_eq!(
            err,
            "party `payee` has the same argument name as parameter `Payee`"
        );

        // A bound value is still an argument of the same name.
        let mut file = minimal_tii(json!({
            "pay": tx(json!({ "type": "object", "properties": { "fee": { "type": "integer" } } }))
        }));
        file.environment =
            Some(json!({ "type": "object", "properties": { "fee": { "type": "integer" } } }));
        file.profiles.get_mut("preprod").unwrap().environment = json!({ "fee": 5 });
        let err = input_schema(&file, "pay", "preprod").unwrap_err();
        assert!(err.contains("environment field `fee`"), "{err}");
    }

    #[test]
    fn non_object_params_are_errors() {
        let file = minimal_tii(json!({ "pay": tx(json!({ "type": "array" })) }));
        let err = input_schema(&file, "pay", "preprod").unwrap_err();
        assert!(err.contains("object schema"), "{err}");

        let file = minimal_tii(json!({
            "pay": tx(json!({ "properties": { "x": { "$ref": "#/components/schemas/Nope" } } }))
        }));
        let err = input_schema(&file, "pay", "preprod").unwrap_err();
        assert_eq!(
            err,
            "params: unresolved $ref `#/components/schemas/Nope` at `#/properties/x`"
        );
    }

    #[test]
    fn unknown_transactions_and_profiles_are_errors() {
        let file = minimal_tii(json!({ "pay": tx(json!({})) }));
        assert!(input_schema(&file, "swap", "preprod").is_err());
        assert!(input_schema(&file, "pay", "mainnet").is_err());
    }

    fn descriptor(name: &str, slug: Option<&str>) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            title: String::new(),
            description: String::new(),
            input_schema: json!({}),
            output_schema: json!({}),
            annotations: ToolAnnotations::FIXED,
            registration_slug: slug.map(str::to_string),
            tx_name: None,
        }
    }

    #[test]
    fn duplicate_tool_names_across_registrations_are_errors() {
        // Slugs are unique, but `abc` + `def_x` and `abc_def` + `x` still meet.
        let tools = [
            descriptor("abc_def_x", Some("abc")),
            descriptor("abc_def_x", Some("abc_def")),
        ];
        let err = check_unique(&tools).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::RegistrationUnavailable);
        assert!(
            err.to_string().contains(
                "tool name `abc_def_x` is claimed by both registration `abc` and registration \
                 `abc_def`"
            ),
            "{err}"
        );

        let mut tools = fixed_tools();
        tools.push(descriptor(GET_SKILL_TOOL, Some("fluent")));
        let err = check_unique(&tools).unwrap_err();
        assert!(err.to_string().contains("a fixed tool"), "{err}");
    }

    #[test]
    fn output_schemas_have_no_refs_or_defs() {
        for schema in [
            schema_of::<PreparedTransaction>(),
            schema_of::<SkillResult>(),
        ] {
            let text = schema.to_string();
            assert!(!text.contains("$ref"), "{text}");
            assert!(!text.contains("$defs"), "{text}");
            assert_eq!(schema["type"], json!("object"));
        }
    }
}
