//! The Streamable HTTP transport: the [`FluentHandler`] at `/mcp`, behind
//! [`auth`].
//!
//! | Route | Authenticated | Serves |
//! |---|---|---|
//! | `/mcp` | yes | MCP over Streamable HTTP, one [`FluentHandler`] per session |
//! | `GET /healthz` | no | `{"status": "ok", "version": …}` |
//! | `GET /.well-known/oauth-protected-resource` | no | RFC 9728 metadata, `oidc` mode only |
//!
//! Sessions live in memory. A request body may be at most
//! [`MAX_BODY_BYTES`]. The `Host` header must name a loopback address, the
//! listen address or the public URL's host.

pub mod auth;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router, middleware};
use fluent_core::Config;
use http::StatusCode;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::mcp::FluentHandler;
use auth::{Authenticator, METADATA_PATH};

/// The largest request body accepted, in bytes.
pub const MAX_BODY_BYTES: usize = 256 * 1024;

/// A bound HTTP server, not yet serving.
pub struct HttpServer {
    listener: TcpListener,
    router: Router,
    cancel: CancellationToken,
}

impl HttpServer {
    /// Binds `[server].listen` and builds the routes for `handler`.
    ///
    /// Fails before binding when `auth.mode = "none"` and the listen address
    /// is not loopback, or when the authenticator cannot be built (see
    /// [`Authenticator::new`]).
    pub async fn bind(config: &Config, handler: FluentHandler) -> anyhow::Result<HttpServer> {
        let listen = config.server.listen;
        let public_url = config.server.public_url.as_deref();
        let auth = Authenticator::new(&config.auth, public_url)?;
        if auth.is_open() && !listen.ip().is_loopback() {
            anyhow::bail!(
                "refusing to serve {listen} with auth.mode = \"none\": \
                 bind a loopback address or configure authentication"
            );
        }

        let cancel = CancellationToken::new();
        let router = router(
            handler,
            auth,
            allowed_hosts(listen, public_url),
            cancel.clone(),
        );
        let listener = TcpListener::bind(listen)
            .await
            .with_context(|| format!("binding {listen}"))?;
        Ok(HttpServer {
            listener,
            router,
            cancel,
        })
    }

    /// The address actually bound, which differs from the configured one
    /// when that asked for port 0.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves until `shutdown` completes, then ends every MCP session and
    /// waits for requests in flight.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let cancel = self.cancel;
        axum::serve(self.listener, self.router)
            .with_graceful_shutdown(async move {
                shutdown.await;
                cancel.cancel();
            })
            .await
            .context("serving HTTP")
    }
}

/// The routes: `/mcp` behind `auth`, then the public ones.
pub fn router(
    handler: FluentHandler,
    auth: Authenticator,
    allowed_hosts: Vec<String>,
    cancel: CancellationToken,
) -> Router {
    let auth = Arc::new(auth);
    let handler = Arc::new(handler);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts)
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .with_cancellation_token(cancel);
    let mcp = StreamableHttpService::new(
        move || Ok(handler.for_session()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let protected = Router::new()
        .route_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(
            Arc::clone(&auth),
            auth::require_auth,
        ));
    Router::new()
        .route("/healthz", get(healthz))
        .route(METADATA_PATH, get(metadata))
        // RFC 9728 §3.1: the metadata of resource `/mcp` may also be looked
        // up with the resource's path appended.
        .route(&format!("{METADATA_PATH}/mcp"), get(metadata))
        .with_state(auth)
        .merge(protected)
}

/// The `Host` values the MCP endpoint accepts: loopback names, the listen
/// address unless it is unspecified, and the public URL's host.
fn allowed_hosts(listen: SocketAddr, public_url: Option<&str>) -> Vec<String> {
    let mut hosts: Vec<String> = ["localhost", "127.0.0.1", "::1"]
        .into_iter()
        .map(String::from)
        .collect();
    if !listen.ip().is_unspecified() && !listen.ip().is_loopback() {
        hosts.push(listen.ip().to_string());
    }
    if let Some(host) = public_url.and_then(url_host) {
        hosts.push(host.to_string());
    }
    hosts
}

/// The host of an `http(s)://` URL, without port or brackets.
fn url_host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next()?,
        None => authority.split(':').next()?,
    };
    (!host.is_empty()).then_some(host)
}

async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

async fn metadata(State(auth): State<Arc<Authenticator>>) -> Response {
    match auth.metadata() {
        Some(metadata) => Json(metadata.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Completes on SIGTERM or Ctrl-C.
pub async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => tracing::info!("interrupted; shutting down"),
        () = terminate => tracing::info!("SIGTERM received; shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_host_strips_scheme_port_and_path() {
        assert_eq!(url_host("https://fluent.tx3.land"), Some("fluent.tx3.land"));
        assert_eq!(url_host("http://example.com:8080/x"), Some("example.com"));
        assert_eq!(url_host("http://[::1]:80/"), Some("::1"));
        assert_eq!(url_host("fluent.tx3.land"), None);
    }

    #[test]
    fn allowed_hosts_add_the_listen_address_and_public_host() {
        let listen: SocketAddr = "10.0.0.5:8080".parse().unwrap();
        let hosts = allowed_hosts(listen, Some("https://fluent.tx3.land/"));
        assert!(hosts.contains(&"10.0.0.5".to_string()));
        assert!(hosts.contains(&"fluent.tx3.land".to_string()));

        let any: SocketAddr = "0.0.0.0:8080".parse().unwrap();
        assert!(!allowed_hosts(any, None).contains(&"0.0.0.0".to_string()));
    }
}
