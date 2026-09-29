//! Tool calls are traced with identifiers and argument names, and nothing
//! logged while calling them over HTTP contains an argument value, an
//! address, the bearer token, the caller's subject or the TRP API key.
//!
//! This file holds a single test so it can install a process-wide JSON
//! subscriber with `fluent serve`'s filter, capturing every other event at
//! `trace` from every thread.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use fluent_server::limits::{Gate, Limits, Quota};
use fluent_server::logging;
use fluent_server::mcp::sub_hash;
use fluent_server::store::Store;
use serde_json::{Value, json};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUBJECT: &str = "sentinel-subject-7f3a";
const API_KEY: &str = "sentinel-trp-key-c4d2";
const QUANTITY: u64 = 424_242_424;
const BAD_QUANTITY: &str = "sentinel-quantity-5e1b";

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
async fn tool_calls_are_traced_without_values_or_credentials() {
    let captured = Captured::default();
    let writer = captured.clone();
    let logs = tracing_subscriber::fmt::layer()
        .json()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .with_filter(LevelFilter::TRACE)
        .with_filter(logging::no_message_bodies());
    tracing_subscriber::registry().with(logs).init();

    let (key, _) = keys();
    let issuer = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks(&[key])))
        .mount(&issuer)
        .await;
    let trp = resolver(Duration::ZERO).await;
    let text = limited_config_text(&trp.uri(), "")
        .replace(
            "[server]\n",
            &format!("[server]\npublic_url = \"{PUBLIC_URL}\"\n"),
        )
        .replace(
            "mode = \"token\"\ntoken_env = \"FLUENT_API_TOKEN\"",
            &oidc_auth(&format!("{}/jwks.json", issuer.uri())),
        );
    let config = load_config(&text, &[("TRP_PREPROD_API_KEY", API_KEY)]);
    let dir = tempfile::tempdir().expect("temp dir");
    let store = Store::open(&dir.path().join("fluent.sqlite"))
        .await
        .expect("open store");
    let limits = Limits {
        gate: Gate::default(),
        quota: Some(Quota::new(store, 2)),
    };
    let server = start_limited(&config, limits).await;
    let token = key.sign(&claims(SUBJECT));
    let (status, session) = server.initialize(Some(&token)).await;
    assert_eq!(status, 200);
    let session = session.expect("a session id");

    let mut args = transfer_args();
    args["quantity"] = json!(QUANTITY);
    let prepared = call(&server, &token, &session, TRANSFER_TOOL, args.clone()).await;
    assert_eq!(prepared["status"], "prepared_unsigned", "{prepared}");
    let mut invalid = args.clone();
    invalid["quantity"] = json!(BAD_QUANTITY);
    let rejected = call(&server, &token, &session, TRANSFER_TOOL, invalid).await;
    assert_eq!(rejected["error"]["code"], "invalid_arguments", "{rejected}");
    let exhausted = call(&server, &token, &session, TRANSFER_TOOL, args).await;
    assert_eq!(exhausted["error"]["code"], "quota_exhausted", "{exhausted}");
    let address = call(
        &server,
        &token,
        &session,
        "fluent_inspect_address",
        json!({ "address": RECEIVER }),
    )
    .await;
    assert_eq!(address["input"], RECEIVER);

    let logs =
        String::from_utf8_lossy(&captured.0.lock().expect("the log buffer lock")).into_owned();
    let finished: Vec<Value> = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event["fields"]["message"]
                .as_str()
                .is_some_and(|m| m.starts_with("tool call "))
        })
        .collect();
    let outcomes: Vec<&str> = finished
        .iter()
        .map(|event| event["fields"]["outcome"].as_str().expect("an outcome"))
        .collect();
    assert_eq!(
        outcomes,
        ["ok", "invalid_arguments", "quota_exhausted", "ok"],
        "{logs}"
    );
    let span = &finished[0]["span"];
    assert_eq!(span["name"], "tool_call", "{span}");
    assert_eq!(span["tool"], TRANSFER_TOOL);
    assert_eq!(span["registration"], "transfer_preprod");
    assert_eq!(span["tx"], "transfer");
    assert_eq!(span["sub_hash"], sub_hash(SUBJECT));
    let arguments = span["arguments"].as_str().expect("argument names");
    for name in ["quantity", "sender", "receiver", "middleman"] {
        assert!(arguments.contains(name), "{arguments}");
    }
    assert!(
        finished[0]["fields"]["duration_ms"].is_u64(),
        "{}",
        finished[0]
    );

    let jwt_signature = token.rsplit('.').next().expect("a JWT signature");
    for (name, secret) in [
        ("sender address", SENDER),
        ("receiver address", RECEIVER),
        ("rejected quantity", BAD_QUANTITY),
        ("quantity", &QUANTITY.to_string()),
        ("subject", SUBJECT),
        ("email", &format!("{SUBJECT}@example.com")),
        ("bearer token", &token),
        ("JWT signature", jwt_signature),
        ("TRP API key", API_KEY),
    ] {
        if let Some(line) = logs.lines().find(|line| line.contains(secret)) {
            let line: String = line.chars().take(600).collect();
            panic!("the logs contain the {name}: {line}");
        }
    }
}
