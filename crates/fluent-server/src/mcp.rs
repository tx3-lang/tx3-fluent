//! The MCP server: the catalog's tools, listed and called over any rmcp
//! transport.
//!
//! [`FluentHandler`] implements [`ServerHandler`] by hand, because its tools
//! come from the registrations loaded at startup rather than from code.
//! `tools/list` returns the [`ToolDescriptor`]s the session's [`ToolScope`]
//! can see, which [`Scoping`] chooses when the session initializes;
//! `tools/call` dispatches by name:
//!
//! - a transaction tool → [`Engine::prepare`];
//! - `fluent_get_skill` → the registration's skill, as a [`SkillResult`];
//! - `fluent_inspect_address` → [`address::inspect`].
//!
//! A result carries the JSON value both as `structuredContent` and as one
//! text content. A [`FluentError`] is a tool result with `isError: true` and
//! the text `{"error": {code, message, details}}`, never a JSON-RPC error; only
//! a tool name no registration defines is a protocol error. A transaction tool
//! outside the session's scope fails as `registration_unavailable`, and every
//! call of a session whose scope refuses it fails as `unauthorized`.
//!
//! When [`Scoping::changes`] announces the session's subject, the session is
//! sent `notifications/tools/list_changed`.
//!
//! Over HTTP, every session gets its own handler from
//! [`FluentHandler::for_session`]. It records the [`Principal`] that
//! initialized the session and refuses requests authenticated as anyone
//! else.
//!
//! Transaction tools run under the handler's [`Limits`]: the session
//! principal's daily quota, the server-wide concurrency gate and the global
//! cutoff. Each tool call is traced in a `tool_call` span carrying the tool,
//! `sub_hash` (a SHA-256 prefix of the principal's `sub`), `registration`,
//! `tx`, the argument *names*, `outcome` (`ok` or the error code) and
//! `duration_ms`; never an argument value. A call to a tool the catalog does
//! not have is logged with outcome `unknown_tool`. Transaction tool calls are
//! also [measured](crate::metrics).
//!
//! Each `tools/list` answered is logged at `info` with `sub_hash` and the
//! listed tool names, so a session's view of the catalog can be read back
//! from the logs.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use fluent_core::catalog::{
    self, GET_SKILL_TOOL, INSPECT_ADDRESS_TOOL, SkillProtocol, SkillResult, ToolDescriptor,
};
use fluent_core::{Catalog, Engine, FluentError, PrepareRequest, Registration, address};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, JsonObject, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::{Peer, RequestContext};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::field::Empty;
use tracing::{Instrument, info, info_span, warn};

use crate::http::auth::Principal;
use crate::limits::Limits;
use crate::metrics;

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
    /// The slugs of the visible registrations, asked on every request; fails
    /// as [`FluentError::Unauthorized`] when the session may see and call
    /// nothing.
    fn visible_slugs(&self) -> BoxFuture<'_, Result<Vec<String>, FluentError>>;
}

/// Chooses each session's [`ToolScope`] from the principal that initialized
/// it.
pub trait Scoping: Send + Sync + 'static {
    /// The scope of a session `principal` initialized; `None` over stdio and
    /// without authentication. Failing fails the `initialize` request.
    fn scope_for<'a>(
        &'a self,
        principal: Option<&'a Principal>,
    ) -> BoxFuture<'a, Result<Arc<dyn ToolScope>, FluentError>>;

    /// Announces the subjects whose scope changed; `None` when scopes never
    /// change.
    fn changes(&self) -> Option<broadcast::Receiver<String>> {
        None
    }
}

/// Every loaded registration, for every session: the scope of a self-hosted
/// server.
#[derive(Debug, Clone)]
pub struct AllRegistrations {
    slugs: Arc<[String]>,
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
    fn visible_slugs(&self) -> BoxFuture<'_, Result<Vec<String>, FluentError>> {
        std::future::ready(Ok(self.slugs.to_vec())).boxed()
    }
}

impl Scoping for AllRegistrations {
    fn scope_for<'a>(
        &'a self,
        _principal: Option<&'a Principal>,
    ) -> BoxFuture<'a, Result<Arc<dyn ToolScope>, FluentError>> {
        let scope: Arc<dyn ToolScope> = Arc::new(self.clone());
        std::future::ready(Ok(scope)).boxed()
    }
}

/// The MCP server over one loaded [`Catalog`].
pub struct FluentHandler {
    catalog: Arc<Catalog>,
    engine: Arc<Engine>,
    scoping: Arc<dyn Scoping>,
    tools: Arc<[ToolDescriptor]>,
    limits: Limits,
    /// Who initialized this session; `None` inside when nobody authenticated.
    principal: OnceLock<Option<Principal>>,
    /// What this session sees, chosen at `initialize`.
    scope: OnceLock<Arc<dyn ToolScope>>,
    /// Stops the session's change watcher when the session's handler drops.
    watcher: OnceLock<DropGuard>,
    /// Counts the session as active from `initialize` until it drops.
    active: OnceLock<metrics::Session>,
}

impl FluentHandler {
    /// Builds the handler and computes the catalog's tools once. Its
    /// transaction tools are gated by the default `[limits]` and unmetered;
    /// see [`FluentHandler::with_limits`].
    ///
    /// Fails as [`catalog::all_tools`] does: when a registration's tools
    /// cannot be built or two tools share a name.
    pub fn new(
        catalog: Arc<Catalog>,
        engine: Arc<Engine>,
        scoping: Arc<dyn Scoping>,
    ) -> Result<FluentHandler, FluentError> {
        let tools = catalog::all_tools(&catalog)?.into();
        Ok(FluentHandler {
            catalog,
            engine,
            scoping,
            tools,
            limits: Limits::default(),
            principal: OnceLock::new(),
            scope: OnceLock::new(),
            watcher: OnceLock::new(),
            active: OnceLock::new(),
        })
    }

    /// Applies `limits` to transaction tools, in this handler and every
    /// session's.
    pub fn with_limits(self, limits: Limits) -> FluentHandler {
        FluentHandler { limits, ..self }
    }

    /// A handler for a new session: the same catalog, engine, scoping, tools
    /// and limits, and no principal or scope yet.
    pub fn for_session(&self) -> FluentHandler {
        FluentHandler {
            catalog: Arc::clone(&self.catalog),
            engine: Arc::clone(&self.engine),
            scoping: Arc::clone(&self.scoping),
            tools: Arc::clone(&self.tools),
            limits: self.limits.clone(),
            principal: OnceLock::new(),
            scope: OnceLock::new(),
            watcher: OnceLock::new(),
            active: OnceLock::new(),
        }
    }

    /// Who initialized this session: `None` before `initialize`, over stdio
    /// and when the server authenticates nobody.
    pub fn principal(&self) -> Option<&Principal> {
        self.principal.get().and_then(Option::as_ref)
    }

    /// Refuses a request authenticated as someone other than the session's
    /// principal.
    fn check_principal(&self, context: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
        match self.principal.get() {
            Some(session) if *session != request_principal(context) => {
                info!("request refused: the session belongs to another principal");
                Err(ErrorData::invalid_request(
                    "this session belongs to another principal",
                    None,
                ))
            }
            _ => Ok(()),
        }
    }

    /// Every tool of the catalog, whoever can see it.
    pub fn tools(&self) -> &[ToolDescriptor] {
        &self.tools
    }

    /// The slugs this session's scope shows; nothing before `initialize`.
    async fn visible_slugs(&self) -> Result<Vec<String>, FluentError> {
        match self.scope.get() {
            Some(scope) => scope.visible_slugs().await,
            None => Err(FluentError::Unauthorized),
        }
    }

    /// The tools this session can see: the fixed tools, then the transaction
    /// tools of the visible registrations. Nothing when the scope refuses the
    /// session.
    pub async fn visible_tools(&self) -> Result<Vec<&ToolDescriptor>, FluentError> {
        let visible = match self.visible_slugs().await {
            Ok(visible) => visible,
            Err(FluentError::Unauthorized) => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        Ok(self
            .tools
            .iter()
            .filter(|tool| match &tool.registration_slug {
                None => true,
                Some(slug) => visible.contains(slug),
            })
            .collect())
    }

    /// Calls tool `name`; `None` when no registration defines it. A tool
    /// outside the session's scope fails as
    /// [`FluentError::RegistrationUnavailable`]; a transaction tool runs
    /// under the handler's [`Limits`], counted against the session
    /// principal.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
    ) -> Option<Result<Value, FluentError>> {
        let tool = self.tools.iter().find(|t| t.name == name)?;
        let visible = match self.visible_slugs().await {
            Ok(visible) => visible,
            Err(err) => return Some(Err(err)),
        };
        if let Some(slug) = &tool.registration_slug
            && !visible.contains(slug)
        {
            return Some(Err(FluentError::RegistrationUnavailable {
                registration: slug.clone(),
                reason: "it is not selected for this account, or changed since it was \
                         selected; select it again"
                    .to_string(),
            }));
        }
        let result = match (&tool.registration_slug, &tool.tx_name) {
            (Some(slug), Some(tx)) => {
                let preparation = self.engine.prepare(PrepareRequest {
                    registration: slug.clone(),
                    tx: tx.clone(),
                    args: Value::Object(args),
                });
                let sub = self.principal().map(|p| p.sub.as_str());
                self.limits.apply(sub, preparation).await.and_then(to_json)
            }
            _ if name == GET_SKILL_TOOL => only_string_arg(&args, "protocol")
                .and_then(|p| self.skill(p, &visible).and_then(to_json)),
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
    fn skill(&self, protocol: &str, visible: &[String]) -> Result<SkillResult, FluentError> {
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

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let principal = request_principal(&context);
        if self.principal.set(principal).is_err() {
            self.check_principal(&context)?;
        }
        if self.scope.get().is_none() {
            let scope = self
                .scoping
                .scope_for(self.principal())
                .await
                .map_err(|err| {
                    warn!(code = %err.code(), "choosing the session's scope failed: {err}");
                    ErrorData::internal_error("the session's tools are unavailable", None)
                })?;
            if self.scope.set(scope).is_ok() {
                let _ = self.active.set(metrics::Session::start());
                if let (Some(changes), Some(principal)) = (self.scoping.changes(), self.principal())
                {
                    let _ = self.watcher.set(watch_changes(
                        changes,
                        principal.sub.clone(),
                        context.peer.clone(),
                    ));
                }
            }
        }
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.check_principal(&context)?;
        let tools = self.visible_tools().await.map_err(|err| {
            warn!(code = %err.code(), "listing the session's tools failed: {err}");
            ErrorData::internal_error("the session's tools are unavailable", None)
        })?;
        info!(
            sub_hash = self.principal().map(|p| sub_hash(&p.sub)),
            tools = %tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(","),
            count = tools.len(),
            "listed the session's tools"
        );
        Ok(ListToolsResult::with_all_items(
            tools.into_iter().map(to_tool).collect(),
        ))
    }

    /// Any tool of the catalog: the transport reads input schemas through a
    /// handler that serves no session.
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).map(to_tool)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.check_principal(&context)?;
        let name = request.name.as_ref();
        let args = request.arguments.unwrap_or_default();
        let descriptor = self.tools.iter().find(|t| t.name == name);
        let (registration, tx) = descriptor
            .map(|d| (d.registration_slug.as_deref(), d.tx_name.as_deref()))
            .unwrap_or_default();
        // Identifiers and argument names only: never an argument value.
        let span = info_span!(
            "tool_call",
            tool = %name,
            sub_hash = Empty,
            registration = Empty,
            tx = Empty,
            arguments = %args.keys().map(String::as_str).collect::<Vec<_>>().join(","),
            outcome = Empty,
            duration_ms = Empty,
        );
        if let Some(principal) = self.principal() {
            span.record("sub_hash", sub_hash(&principal.sub));
        }
        if let (Some(registration), Some(tx)) = (registration, tx) {
            span.record("registration", registration);
            span.record("tx", tx);
        }

        let started = Instant::now();
        let Some(result) = self.call(name, args).instrument(span.clone()).await else {
            let outcome = "unknown_tool";
            span.record("outcome", outcome);
            span.record("duration_ms", 0_u64);
            span.in_scope(|| info!(tool = %name, outcome, "unknown tool"));
            return Err(ErrorData::invalid_params(
                format!("unknown tool `{name}`"),
                None,
            ));
        };
        let elapsed = started.elapsed();
        let outcome = match &result {
            Ok(_) => "ok",
            Err(err) => err.code().as_str(),
        };
        let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        span.record("outcome", outcome);
        span.record("duration_ms", duration_ms);
        if let (Some(registration), Some(tx)) = (registration, tx) {
            metrics::prepared(registration, tx, outcome, elapsed);
        }
        let result = span.in_scope(|| match result {
            Ok(value) => {
                info!(tool = %name, outcome, duration_ms, "tool call succeeded");
                CallToolResult::structured(value)
            }
            Err(err) => {
                info!(tool = %name, code = %err.code(), outcome, duration_ms, "tool call failed");
                CallToolResult::error(vec![ContentBlock::text(error_json(&err).to_string())])
            }
        });
        Ok(result.into())
    }
}

/// Sends `peer` `notifications/tools/list_changed` whenever `changes`
/// announces `sub`, until the returned guard drops.
fn watch_changes(
    mut changes: broadcast::Receiver<String>,
    sub: String,
    peer: Peer<RoleServer>,
) -> DropGuard {
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    tokio::spawn(async move {
        loop {
            let changed = tokio::select! {
                () = stopped.cancelled() => return,
                changed = changes.recv() => changed,
            };
            match changed {
                Ok(changed) if changed != sub => continue,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
            if peer.notify_tool_list_changed().await.is_err() {
                info!("tools/list_changed not delivered");
            }
        }
    });
    stop.drop_guard()
}

/// The principal authentication attached to the HTTP request behind
/// `context`; `None` over stdio or without authentication.
pub fn request_principal(context: &RequestContext<RoleServer>) -> Option<Principal> {
    context
        .extensions
        .get::<http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Principal>())
        .cloned()
}

/// The first 12 hex digits of the SHA-256 of `sub`: enough to follow one
/// user through the logs without naming them.
pub fn sub_hash(sub: &str) -> String {
    let digest = Sha256::digest(sub.as_bytes());
    digest.iter().take(6).fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
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
