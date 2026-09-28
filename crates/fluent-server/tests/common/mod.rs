//! Shared by the HTTP tests: an in-process server over the fixture bundles,
//! RSA signing keys and their JWKS, token minting and MCP requests.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluent_core::registration::load_dir;
use fluent_core::{Config, Engine};
use fluent_server::http::HttpServer;
use fluent_server::mcp::{AllRegistrations, FluentHandler};
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::RsaPrivateKey;
use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
use serde_json::{Value, json};
use tokio::sync::oneshot;

pub const ISSUER: &str = "https://issuer.test/";
pub const AUDIENCE: &str = "https://fluent.test/mcp";
pub const PUBLIC_URL: &str = "https://fluent.test";
pub const CHALLENGE: &str =
    "Bearer resource_metadata=\"https://fluent.test/.well-known/oauth-protected-resource\"";

/// A 2048-bit RSA signing key and the `kid` it is published under.
pub struct SigningKey {
    pub kid: &'static str,
    pub encoding: EncodingKey,
    pub jwk: Jwk,
}

impl SigningKey {
    fn generate(kid: &'static str) -> SigningKey {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).expect("generate RSA key");
        let pem = private
            .to_pkcs1_pem(LineEnding::LF)
            .expect("encode RSA key");
        let encoding = EncodingKey::from_rsa_pem(pem.as_bytes()).expect("load RSA key");
        let mut jwk = Jwk::from_encoding_key(&encoding, Algorithm::RS256).expect("RSA JWK");
        jwk.common.key_id = Some(kid.to_string());
        SigningKey { kid, encoding, jwk }
    }

    /// Signs `claims` with this key, naming `kid` in the header.
    pub fn sign_as(&self, kid: &str, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.to_string());
        jsonwebtoken::encode(&header, claims, &self.encoding).expect("sign token")
    }

    /// Signs `claims` with this key under its own `kid`.
    pub fn sign(&self, claims: &Value) -> String {
        self.sign_as(self.kid, claims)
    }
}

/// Two keys, generated once per test binary: `a` and `b`.
pub fn keys() -> &'static (SigningKey, SigningKey) {
    static KEYS: OnceLock<(SigningKey, SigningKey)> = OnceLock::new();
    KEYS.get_or_init(|| (SigningKey::generate("a"), SigningKey::generate("b")))
}

/// A JWKS publishing `keys`.
pub fn jwks(keys: &[&SigningKey]) -> Value {
    let set = JwkSet {
        keys: keys.iter().map(|k| k.jwk.clone()).collect(),
    };
    serde_json::to_value(set).expect("serialize JWKS")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
}

/// Claims that pass validation, for subject `sub`.
pub fn claims(sub: &str) -> Value {
    json!({
        "iss": ISSUER,
        "aud": AUDIENCE,
        "sub": sub,
        "email": format!("{sub}@example.com"),
        "iat": now(),
        "exp": now() + 3600,
    })
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fluent-core/tests/fixtures")
}

/// A configuration serving the valid fixture bundles on `listen`, with
/// `auth` as the `[auth]` table body and `server` appended to `[server]`.
pub fn config_text(listen: &str, server: &str, auth: &str) -> String {
    let dir = fixtures().join("registrations/valid");
    format!(
        "[server]\nlisten = \"{listen}\"\n{server}\n\n\
         [registrations]\ndir = {}\n\n\
         [networks.preprod]\ntrp_url = \"http://127.0.0.1:9\"\n\n\
         [auth]\n{auth}\n",
        toml::Value::String(dir.display().to_string())
    )
}

pub fn oidc_auth(jwks_url: &str) -> String {
    format!(
        "mode = \"oidc\"\nissuer = \"{ISSUER}\"\njwks_url = \"{jwks_url}\"\naudience = \"{AUDIENCE}\""
    )
}

pub fn load_config(text: &str, env: &[(&str, &str)]) -> Config {
    Config::from_toml_str(text, env.iter().copied()).expect("valid configuration")
}

async fn handler(config: &Config) -> FluentHandler {
    let registrations = load_dir(&config.registrations.dir)
        .await
        .expect("load registrations");
    assert!(registrations.rejected.is_empty());
    let catalog = Arc::new(registrations.catalog);
    let engine = Arc::new(Engine::new(config, &catalog));
    let scope = Arc::new(AllRegistrations::new(&catalog));
    FluentHandler::new(catalog, engine, scope).expect("build handler")
}

/// Binds `config` the way `fluent serve --http` does.
pub async fn bind(config: &Config) -> anyhow::Result<HttpServer> {
    HttpServer::bind(config, handler(config).await).await
}

/// A server running in the background until dropped.
pub struct Running {
    pub base: String,
    pub client: reqwest::Client,
    stop: Option<oneshot::Sender<()>>,
}

impl Running {
    pub async fn start(config: &Config) -> Running {
        let server = bind(config).await.expect("bind");
        let addr: SocketAddr = server.local_addr().expect("local address");
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(server.run(async {
            let _ = stopped.await;
        }));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP client");
        Running {
            base: format!("http://{addr}"),
            client,
            stop: Some(stop),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// POSTs one JSON-RPC message to `/mcp`, with `token` as the bearer and
    /// `session` as the MCP session.
    pub async fn post_mcp(
        &self,
        message: &Value,
        token: Option<&str>,
        session: Option<&str>,
    ) -> reqwest::Response {
        let mut request = self
            .client
            .post(self.url("/mcp"))
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(message.to_string());
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(session) = session {
            request = request
                .header("mcp-session-id", session)
                .header("mcp-protocol-version", PROTOCOL_VERSION);
        }
        request.send().await.expect("POST /mcp")
    }

    /// Initializes a session as `token`; the response and its session id.
    pub async fn initialize(&self, token: Option<&str>) -> (reqwest::StatusCode, Option<String>) {
        let response = self.post_mcp(&initialize(), token, None).await;
        let status = response.status();
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        if status.is_success() {
            let reply = rpc_reply(response).await;
            assert!(reply.get("result").is_some(), "{reply}");
            let session = session.as_deref().expect("a session id");
            let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
            let ack = self.post_mcp(&initialized, token, Some(session)).await;
            assert!(ack.status().is_success(), "{}", ack.status());
        }
        (status, session)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "fluent-http-test", "version": "0"}
        }
    })
}

pub fn list_tools() -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
}

/// The JSON-RPC reply in a response body, sent as JSON or as an SSE event.
pub async fn rpc_reply(response: reqwest::Response) -> Value {
    let body = response.text().await.expect("response body");
    if let Ok(value) = serde_json::from_str::<Value>(&body) {
        return value;
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
        .find(|message| message.get("id").is_some())
        .unwrap_or_else(|| panic!("no JSON-RPC reply in {body:?}"))
}
