//! No bearer token, static or JWT, appears in anything the HTTP transport,
//! rmcp, axum or the JWKS client logs while authenticating.
//!
//! This file holds a single test so it can install a process-wide
//! subscriber that captures every event at `trace`, from every thread.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const STATIC_TOKEN: &str = "sentinel-static-token-4b7e";

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the log buffer lock")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn no_token_value_is_logged() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    // Token mode: the right token, a near miss and a wrong one.
    let text = config_text(
        "127.0.0.1:0",
        "",
        "mode = \"token\"\ntoken_env = \"FLUENT_API_TOKEN\"",
    );
    let server = Running::start(&load_config(&text, &[("FLUENT_API_TOKEN", STATIC_TOKEN)])).await;
    let (status, session) = server.initialize(Some(STATIC_TOKEN)).await;
    assert_eq!(status, 200);
    let reply = rpc_reply(
        server
            .post_mcp(&list_tools(), Some(STATIC_TOKEN), session.as_deref())
            .await,
    )
    .await;
    assert!(reply["result"]["tools"].is_array(), "{reply}");
    let near_miss = format!("{STATIC_TOKEN}x");
    for token in [near_miss.as_str(), "sentinel-wrong-token-0d2c"] {
        let (status, _) = server.initialize(Some(token)).await;
        assert_eq!(status, 401);
    }

    // OIDC mode: an accepted JWT, an expired one and one signed by an
    // unpublished key.
    let (a, b) = keys();
    let issuer = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[a])))
        .mount(&issuer)
        .await;
    let text = config_text(
        "127.0.0.1:0",
        &format!("public_url = \"{PUBLIC_URL}\""),
        &oidc_auth(&format!("{}/jwks.json", issuer.uri())),
    );
    let server = Running::start(&load_config(&text, &[])).await;
    let good = a.sign(&claims("alice"));
    let mut expired = claims("alice");
    expired["exp"] = json!(now() - 3600);
    let expired = a.sign(&expired);
    let unknown = b.sign(&claims("alice"));
    let (status, session) = server.initialize(Some(&good)).await;
    assert_eq!(status, 200);
    let reply = rpc_reply(
        server
            .post_mcp(&list_tools(), Some(&good), session.as_deref())
            .await,
    )
    .await;
    assert!(reply["result"]["tools"].is_array(), "{reply}");
    for token in [&expired, &unknown] {
        let (status, _) = server.initialize(Some(token)).await;
        assert_eq!(status, 401);
    }

    let logs =
        String::from_utf8_lossy(&captured.0.lock().expect("the log buffer lock")).into_owned();
    assert!(
        logs.contains("request rejected"),
        "the subscriber captured nothing useful: {logs}"
    );
    let jwts = [&good, &expired, &unknown];
    for (name, secret) in [
        ("static token", STATIC_TOKEN),
        ("near-miss token", near_miss.as_str()),
        ("wrong token", "sentinel-wrong-token-0d2c"),
    ]
    .into_iter()
    .chain(jwts.iter().map(|jwt| ("JWT", jwt.as_str())))
    {
        assert!(!logs.contains(secret), "the logs contain a {name}: {logs}");
    }
    // A JWT's signature alone would be enough to replay it.
    for jwt in jwts {
        let signature = jwt.rsplit('.').next().expect("a JWT signature");
        assert!(
            !logs.contains(signature),
            "the logs contain a JWT signature"
        );
    }
}
