//! The MCP server: the catalog's tools, listed and called over any rmcp
//! transport.
//!
//! [`FluentHandler`] implements [`ServerHandler`] by hand, because its tools
//! come from the registrations loaded at startup rather than from code.
//! `tools/list` returns the [`ToolDescriptor`]s the session's [`ToolScope`]
//! can see; `tools/call` dispatches by name:
//!
//! - a transaction tool → [`Engine::prepare`];
//! - `fluent_get_skill` → the registration's skill, as a [`SkillResult`];
//! - `fluent_inspect_address` → [`address::inspect`].
//!
//! A result carries the JSON value both as `structuredContent` and as one
//! text content. A [`FluentError`] is a tool result with `isError: true` and
//! the text `{"error": {code, message, details}}`, never a JSON-RPC error; only
//! a tool name the session cannot see is a protocol error.

use std::sync::Arc;

use fluent_core::catalog::{
    self, GET_SKILL_TOOL, INSPECT_ADDRESS_TOOL, SkillProtocol, SkillResult, ToolDescriptor,
};
use fluent_core::{Catalog, Engine, FluentError, PrepareRequest, Registration, address};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};
use tracing::info;

/// The server name reported to clients.
pub const SERVER_NAME: &str = "tx3-fluent";

/// The longest [`INSTRUCTIONS`] may be, in characters.
pub const MAX_INSTRUCTIONS_LEN: usize = 512;

/// What the server tells the model about its tools, once per session.
pub const INSTRUCTIONS: &str = "Tx3 Fluent prepares UNSIGNED Cardano transactions for \
    registered protocols. Before calling any protocol tool, call fluent_get_skill for that \
    protocol and follow it. Use fluent_inspect_address to derive credentials and check \
    networks. Tools never sign, submit, or move funds; the user must review and sign in their \
    wallet. If a required value is unavailable, say so instead of guessing.";

/// Selects the registrations one session can see. Transaction tools of any
/// other registration are neither listed nor callable, and
/// `fluent_get_skill` does not find their skills.
pub trait ToolScope: Send + Sync + 'static {
    /// The slugs of the visible registrations.
    fn visible_slugs(&self) -> Vec<String>;
}

/// Every loaded registration: the scope of a self-hosted server.
#[derive(Debug, Clone)]
pub struct AllRegistrations {
    slugs: Vec<String>,
}

impl AllRegistrations {
    /// Sees every registration in `catalog`.
    pub fn new(catalog: &Catalog) -> AllRegistrations {
        AllRegistrations {
            slugs: catalog.iter().map(|r| r.slug().to_string()).collect(),
        }
    }
}

impl ToolScope for AllRegistrations {
    fn visible_slugs(&self) -> Vec<String> {
        self.slugs.clone()
    }
}

/// The MCP server over one loaded [`Catalog`].
pub struct FluentHandler {
    catalog: Arc<Catalog>,
    engine: Arc<Engine>,
    scope: Arc<dyn ToolScope>,
    tools: Vec<ToolDescriptor>,
}

impl FluentHandler {
    /// Builds the handler and computes the catalog's tools once.
    ///
    /// Fails as [`catalog::all_tools`] does: when a registration's tools
    /// cannot be built or two tools share a name.
    pub fn new(
        catalog: Arc<Catalog>,
        engine: Arc<Engine>,
        scope: Arc<dyn ToolScope>,
    ) -> Result<FluentHandler, FluentError> {
        let tools = catalog::all_tools(&catalog)?;
        Ok(FluentHandler {
            catalog,
            engine,
            scope,
            tools,
        })
    }

    /// The tools this session can see: the fixed tools, then the transaction
    /// tools of the visible registrations.
    pub fn visible_tools(&self) -> Vec<&ToolDescriptor> {
        let visible = self.scope.visible_slugs();
        self.tools
            .iter()
            .filter(|tool| match &tool.registration_slug {
                None => true,
                Some(slug) => visible.contains(slug),
            })
            .collect()
    }

    /// Calls the visible tool `name`; `None` when there is none.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
    ) -> Option<Result<Value, FluentError>> {
        let tool = self.visible_tools().into_iter().find(|t| t.name == name)?;
        let result = match (&tool.registration_slug, &tool.tx_name) {
            (Some(slug), Some(tx)) => self
                .engine
                .prepare(PrepareRequest {
                    registration: slug.clone(),
                    tx: tx.clone(),
                    args: Value::Object(args),
                })
                .await
                .and_then(to_json),
            _ if name == GET_SKILL_TOOL => {
                only_string_arg(&args, "protocol").and_then(|p| self.skill(p).and_then(to_json))
            }
            _ if name == INSPECT_ADDRESS_TOOL => only_string_arg(&args, "address")
                .and_then(|a| address::inspect(a).and_then(to_json)),
            _ => Err(FluentError::internal(format!(
                "tool `{name}` has no implementation"
            ))),
        };
        Some(result)
    }

    /// The skill of the visible registration `protocol` names: a slug, or a
    /// `scope/name` that exactly one visible registration serves.
    fn skill(&self, protocol: &str) -> Result<SkillResult, FluentError> {
        let visible = self.scope.visible_slugs();
        let registrations: Vec<&Arc<Registration>> = self
            .catalog
            .iter()
            .filter(|r| visible.iter().any(|slug| slug == r.slug()))
            .collect();

        if let Some(registration) = registrations.iter().find(|r| r.slug() == protocol) {
            return Ok(skill_result(registration));
        }
        let matching: Vec<&&Arc<Registration>> = registrations
            .iter()
            .filter(|r| {
                let p = &r.manifest().protocol;
                format!("{}/{}", p.scope, p.name) == protocol
            })
            .collect();
        match matching.as_slice() {
            [] => Err(FluentError::UnknownProtocol {
                protocol: protocol.to_string(),
            }),
            [registration] => Ok(skill_result(registration)),
            several => Err(FluentError::InvalidArguments {
                reason: format!(
                    "protocol {protocol} has several registrations; name one by slug: {}",
                    several
                        .iter()
                        .map(|r| r.slug())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                arguments: vec!["protocol".to_string()],
                violations: Vec::new(),
            }),
        }
    }
}

impl ServerHandler for FluentHandler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.visible_tools().into_iter().map(to_tool).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.visible_tools()
            .into_iter()
            .find(|t| t.name == name)
            .map(to_tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();
        let Some(result) = self.call(name, args).await else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{name}`"),
                None,
            ));
        };
        // Only the tool and the outcome: never an argument value.
        let result = match result {
            Ok(value) => {
                info!(tool = %name, "tool call succeeded");
                CallToolResult::structured(value)
            }
            Err(err) => {
                info!(tool = %name, code = %err.code(), "tool call failed");
                CallToolResult::error(vec![ContentBlock::text(error_json(&err).to_string())])
            }
        };
        Ok(result.into())
    }
}

/// A [`FluentError`] as `{"error": {code, message, details}}`; `details` is
/// left out when there are none.
pub fn error_json(err: &FluentError) -> Value {
    let mut error = json!({ "code": err.code(), "message": err.message() });
    if let Some(details) = err.details() {
        error["details"] = details;
    }
    json!({ "error": error })
}

/// A descriptor as an rmcp [`Tool`].
fn to_tool(descriptor: &ToolDescriptor) -> Tool {
    let hints = descriptor.annotations;
    Tool::new(
        descriptor.name.clone(),
        descriptor.description.clone(),
        Arc::new(object(&descriptor.input_schema)),
    )
    .with_title(descriptor.title.clone())
    .with_raw_output_schema(Arc::new(object(&descriptor.output_schema)))
    .with_annotations(ToolAnnotations::from_raw(
        None,
        Some(hints.read_only_hint),
        Some(hints.destructive_hint),
        Some(hints.idempotent_hint),
        Some(hints.open_world_hint),
    ))
}

/// A schema's top-level object; the catalog only builds object schemas.
fn object(schema: &Value) -> JsonObject {
    schema.as_object().cloned().unwrap_or_default()
}

fn skill_result(registration: &Registration) -> SkillResult {
    let protocol = &registration.manifest().protocol;
    let skill = registration.skill();
    SkillResult {
        protocol: SkillProtocol {
            scope: protocol.scope.clone(),
            name: protocol.name.clone(),
            version: protocol.version.clone(),
            registration_slug: registration.slug().to_string(),
            registration_revision: registration.revision().to_string(),
            network: registration.network(),
        },
        skill_revision: skill.revision,
        dependencies: skill.dependencies.clone(),
        markdown: skill.body.clone(),
    }
}

/// The one string argument `name` of a fixed tool; any other argument is
/// rejected, as its input schema does.
fn only_string_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str, FluentError> {
    let invalid = |reason: String, argument: &str| FluentError::InvalidArguments {
        reason,
        arguments: vec![argument.to_string()],
        violations: Vec::new(),
    };
    if let Some(other) = args.keys().find(|key| *key != name) {
        return Err(invalid(format!("unknown argument `{other}`"), other));
    }
    match args.get(name) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(invalid(format!("`{name}` must be a string"), name)),
        None => Err(invalid(format!("missing required argument `{name}`"), name)),
    }
}

fn to_json(value: impl serde::Serialize) -> Result<Value, FluentError> {
    serde_json::to_value(value).map_err(FluentError::internal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_fit_the_limit() {
        let len = INSTRUCTIONS.chars().count();
        assert!(len <= MAX_INSTRUCTIONS_LEN, "{len} characters");
    }

    #[test]
    fn instructions_read_as_one_paragraph() {
        assert!(!INSTRUCTIONS.contains("  "), "{INSTRUCTIONS}");
        assert!(INSTRUCTIONS.starts_with("Tx3 Fluent prepares UNSIGNED"));
        assert!(INSTRUCTIONS.ends_with("say so instead of guessing."));
    }
}
