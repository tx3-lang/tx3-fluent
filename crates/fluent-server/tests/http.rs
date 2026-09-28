//! `fluent serve --http`, in process: `/mcp` behind each auth mode, the
//! protected resource metadata, `/healthz`, the body limit and the binding
//! rules.

mod common;

use std::time::Duration;

use common::*;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate, Times};

/// A JWKS endpoint publishing `keys`, expecting `fetches` requests.
async fn issuer(keys: &[&SigningKey], fetches: impl Into<Times>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(keys)))
        .expect(fetches)
        .mount(&server)
        .await;
    server
}

async fn oidc_server(issuer: &MockServer) -> Running {
    let text = config_text(
        "127.0.0.1:0",
        &format!("public_url = \"{PUBLIC_URL}/\""),
        &oidc_auth(&format!("{}/jwks.json", issuer.uri())),
    );
    Running::start(&load_config(&text, &[])).await
}

/// Asserts a `401` with the resource metadata challenge and no body.
async fn assert_rejected(response: reqwest::Response, challenge: &str) {
    assert_eq!(response.status(), 401);
    let header = response
        .headers()
        .get("www-authenticate")
        .expect("a WWW-Authenticate header");
    assert_eq!(header, challenge);
    assert_eq!(response.text().await.expect("body"), "");
}

#[tokio::test]
async fn oidc_accepts_a_valid_token_and_lists_tools() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 1).await;
    let server = oidc_server(&issuer).await;
    let token = a.sign(&claims("alice"));

    let (status, session) = server.initialize(Some(&token)).await;
    assert_eq!(status, 200);
    let response = server
        .post_mcp(&list_tools(), Some(&token), session.as_deref())
        .await;
    assert_eq!(response.status(), 200);
    let reply = rpc_reply(response).await;
    let names: Vec<&str> = reply["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(names.contains(&"fluent_get_skill"), "{names:?}");
    assert!(names.contains(&"fluent_inspect_address"), "{names:?}");
}

#[tokio::test]
async fn oidc_accepts_an_audience_list_containing_the_audience() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 1).await;
    let server = oidc_server(&issuer).await;
    let mut claims = claims("alice");
    claims["aud"] = json!(["https://other.test", AUDIENCE]);

    let (status, _) = server.initialize(Some(&a.sign(&claims))).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn oidc_rejects_bad_tokens_with_the_metadata_challenge() {
    let (a, b) = keys();
    let issuer = issuer(&[a], 1..).await;
    let server = oidc_server(&issuer).await;

    let with = |key: &str, value: Value| {
        let mut claims = claims("mallory");
        claims[key] = value;
        claims
    };
    let without = |key: &str| {
        let mut claims = claims("mallory");
        claims.as_object_mut().expect("claims object").remove(key);
        claims
    };
    let hs256 = jsonwebtoken::encode(
        &jsonwebtoken::Header {
            kid: Some("a".to_string()),
            ..jsonwebtoken::Header::default()
        },
        &claims("mallory"),
        &jsonwebtoken::EncodingKey::from_secret(b"not the issuer"),
    )
    .expect("HS256 token");

    let cases: Vec<(&str, Option<String>)> = vec![
        ("no token", None),
        ("not a JWT", Some("not-a-jwt".to_string())),
        (
            "wrong issuer",
            Some(a.sign(&with("iss", json!("https://evil.test/")))),
        ),
        (
            "wrong audience",
            Some(a.sign(&with("aud", json!("https://other.test")))),
        ),
        ("expired", Some(a.sign(&with("exp", json!(now() - 3600))))),
        (
            "not yet valid",
            Some(a.sign(&with("nbf", json!(now() + 3600)))),
        ),
        ("no subject", Some(a.sign(&without("sub")))),
        ("no expiry", Some(a.sign(&without("exp")))),
        ("forged signature", Some(b.sign_as("a", &claims("mallory")))),
        ("unknown kid", Some(b.sign(&claims("mallory")))),
        ("HS256", Some(hs256)),
    ];
    for (case, token) in cases {
        let response = server.post_mcp(&initialize(), token.as_deref(), None).await;
        assert_eq!(response.status(), 401, "{case}");
        assert_rejected(response, CHALLENGE).await;
    }
}

#[tokio::test]
async fn oidc_caches_the_jwks() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 1).await;
    let server = oidc_server(&issuer).await;
    for sub in ["alice", "bob", "carol"] {
        let (status, _) = server.initialize(Some(&a.sign(&claims(sub)))).await;
        assert_eq!(status, 200, "{sub}");
    }
    // `issuer` verifies on drop that the JWKS was fetched once.
}

#[tokio::test]
async fn oidc_refetches_the_jwks_for_an_unknown_kid() {
    let (a, b) = keys();
    let issuer = MockServer::start().await;
    // The first fetch predates key `b`; later ones publish it.
    Mock::given(method("GET"))
        .and(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[a])))
        .up_to_n_times(1)
        .expect(1)
        .mount(&issuer)
        .await;
    Mock::given(method("GET"))
        .and(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[a, b])))
        .expect(1)
        .mount(&issuer)
        .await;
    let server = oidc_server(&issuer).await;
    let token = b.sign(&claims("alice"));

    let (status, _) = server.initialize(Some(&token)).await;
    assert_eq!(status, 401, "b is not published yet");
    // Within the refetch interval, an unknown kid does not fetch again.
    let (status, _) = server.initialize(Some(&token)).await;
    assert_eq!(status, 401);

    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (status, _) = server.initialize(Some(&token)).await;
    assert_eq!(
        status, 200,
        "b is fetched on its first use after the interval"
    );
    let (status, _) = server.initialize(Some(&a.sign(&claims("bob")))).await;
    assert_eq!(status, 200, "a stays cached");
}

#[tokio::test]
async fn a_session_belongs_to_the_principal_that_initialized_it() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 1).await;
    let server = oidc_server(&issuer).await;
    let alice = a.sign(&claims("alice"));
    let bob = a.sign(&claims("bob"));

    let (_, session) = server.initialize(Some(&alice)).await;
    let reply = rpc_reply(
        server
            .post_mcp(&list_tools(), Some(&bob), session.as_deref())
            .await,
    )
    .await;
    assert!(reply.get("error").is_some(), "{reply}");
    assert!(reply.get("result").is_none(), "{reply}");

    let reply = rpc_reply(
        server
            .post_mcp(&list_tools(), Some(&alice), session.as_deref())
            .await,
    )
    .await;
    assert!(reply["result"]["tools"].is_array(), "{reply}");
}

#[tokio::test]
async fn oidc_serves_the_protected_resource_metadata() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 0).await;
    let server = oidc_server(&issuer).await;
    let expected = json!({
        "resource": "https://fluent.test/mcp",
        "authorization_servers": [ISSUER],
        "bearer_methods_supported": ["header"],
        "scopes_supported": [],
    });
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let response = server
            .client
            .get(server.url(path))
            .send()
            .await
            .expect("GET metadata");
        assert_eq!(response.status(), 200, "{path}");
        let body: Value =
            serde_json::from_str(&response.text().await.expect("body")).expect("JSON metadata");
        assert_eq!(body, expected, "{path}");
    }
}

#[tokio::test]
async fn healthz_is_public_and_reports_the_version() {
    let (a, _) = keys();
    let issuer = issuer(&[a], 0).await;
    let server = oidc_server(&issuer).await;
    let response = server
        .client
        .get(server.url("/healthz"))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(response.status(), 200);
    let body: Value =
        serde_json::from_str(&response.text().await.expect("body")).expect("JSON health");
    assert_eq!(
        body,
        json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")})
    );
}

fn token_config(server: &str) -> fluent_core::Config {
    let text = config_text(
        "127.0.0.1:0",
        server,
        "mode = \"token\"\ntoken_env = \"FLUENT_API_TOKEN\"",
    );
    load_config(&text, &[("FLUENT_API_TOKEN", "s3cret-token")])
}

#[tokio::test]
async fn token_mode_accepts_only_the_configured_token() {
    let server = Running::start(&token_config("")).await;

    let (status, session) = server.initialize(Some("s3cret-token")).await;
    assert_eq!(status, 200);
    let reply = rpc_reply(
        server
            .post_mcp(&list_tools(), Some("s3cret-token"), session.as_deref())
            .await,
    )
    .await;
    assert!(reply["result"]["tools"].is_array(), "{reply}");

    for token in [
        None,
        Some("s3cret-tokeN"),
        Some("s3cret-token-plus"),
        Some(""),
    ] {
        let response = server.post_mcp(&initialize(), token, None).await;
        // Without a public URL the challenge names no metadata.
        assert_rejected(response, "Bearer").await;
    }

    // Token mode has no authorization server to advertise.
    let response = server
        .client
        .get(server.url("/.well-known/oauth-protected-resource"))
        .send()
        .await
        .expect("GET metadata");
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn token_mode_challenge_names_the_metadata_under_a_public_url() {
    let server = Running::start(&token_config(&format!("public_url = \"{PUBLIC_URL}\""))).await;
    let response = server.post_mcp(&initialize(), Some("wrong"), None).await;
    assert_rejected(response, CHALLENGE).await;
}

#[tokio::test]
async fn token_mode_requires_the_token_variable() {
    let text = config_text(
        "127.0.0.1:0",
        "",
        "mode = \"token\"\ntoken_env = \"FLUENT_API_TOKEN\"",
    );
    let err = bind(&load_config(&text, &[]))
        .await
        .err()
        .expect("an unset token is refused");
    assert!(err.to_string().contains("FLUENT_API_TOKEN"), "{err}");
}

#[tokio::test]
async fn none_mode_refuses_a_non_loopback_address() {
    for listen in ["0.0.0.0:0", "[::]:0"] {
        let text = config_text(listen, "", "mode = \"none\"");
        let err = bind(&load_config(&text, &[]))
            .await
            .err()
            .expect("an open server on every interface is refused");
        assert!(err.to_string().contains("refusing to serve"), "{err}");
    }
}

#[tokio::test]
async fn none_mode_serves_loopback_without_credentials() {
    let text = config_text("127.0.0.1:0", "", "mode = \"none\"");
    let server = Running::start(&load_config(&text, &[])).await;
    let (status, _) = server.initialize(None).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let server = Running::start(&token_config("")).await;
    let padding = "x".repeat(300 * 1024);
    let mut message = initialize();
    message["params"]["clientInfo"]["name"] = json!(padding);
    let response = server.post_mcp(&message, Some("s3cret-token"), None).await;
    assert_eq!(response.status(), 413);
}
