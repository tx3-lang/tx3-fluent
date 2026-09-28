//! Nothing the engine, the SDK or the HTTP stack logs while preparing
//! contains an argument value or the TRP API key.
//!
//! This file holds a single test so it can install a process-wide
//! subscriber that captures every event at `debug`, from every thread, while
//! a prepare succeeds, while the resolver reports a failure that echoes the
//! caller's address, and while arguments are rejected.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fluent_core::registration::load_dir;
use fluent_core::{Config, Engine, ErrorCode, PrepareRequest};
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const QUANTITY: u64 = 424_242_424;
const OVERRIDE: &str = "override-attempt-9e7b";
const API_KEY: &str = "sentinel-trp-key-51f0";

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

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("the log buffer lock")).into_owned()
    }
}

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn engine(trp_url: &str) -> Engine {
    let text = format!(
        r#"
        [registrations]
        dir = "unused"
        [networks.preprod]
        trp_url = "{trp_url}"
        trp_api_key_env = "TEST_TRP_API_KEY"
        [auth]
        mode = "none"
        "#
    );
    let config = Config::from_toml_str(&text, [("TEST_TRP_API_KEY", API_KEY)])
        .expect("a valid test configuration");
    Engine::new(
        &config,
        &load_dir(fixture("registrations/valid"))
            .expect("the fixtures load")
            .catalog,
    )
}

fn request(args: Value) -> PrepareRequest {
    PrepareRequest {
        registration: "transfer_preprod".into(),
        tx: "transfer".into(),
        args,
    }
}

fn args() -> Value {
    json!({ "quantity": QUANTITY, "sender": SENDER, "receiver": RECEIVER, "middleman": SENDER })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_logs_no_argument_value_or_api_key() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .init();

    let tx_hex = std::fs::read_to_string(fixture("tx/transfer-preprod.hex")).unwrap();
    let success = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": {
                "hash": "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73",
                "tx": tx_hex.trim()
            }
        })))
        .mount(&success)
        .await;
    engine(&success.uri())
        .prepare(request(args()))
        .await
        .unwrap();

    let failure = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": "1",
            "error": {
                "code": -32002,
                "message": "Input not resolved",
                "data": {
                    "name": "source",
                    "query": {
                        "address": SENDER,
                        "collateral": false,
                        "minAmount": { "lovelace": QUANTITY.to_string() },
                        "refs": [],
                        "supportMany": true
                    },
                    "search_space": { "matched": [] }
                }
            }
        })))
        .mount(&failure)
        .await;
    let failing = engine(&failure.uri());
    let err = failing.prepare(request(args())).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InsufficientFunds);

    let mut overriding = args();
    overriding["tax"] = json!(OVERRIDE);
    let err = failing.prepare(request(overriding)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidArguments);
    let mut malformed = args();
    malformed["quantity"] = json!(OVERRIDE);
    let err = failing.prepare(request(malformed)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::InvalidArguments);

    let logs = captured.text();
    assert!(
        logs.contains("resolving transaction") && logs.contains("DEBUG"),
        "the subscriber captured nothing: {logs}"
    );
    for (what, secret) in [
        ("sender address", SENDER),
        ("receiver address", RECEIVER),
        ("quantity", &QUANTITY.to_string()),
        ("rejected value", OVERRIDE),
        ("API key", API_KEY),
    ] {
        assert!(!logs.contains(secret), "{what} reached the logs:\n{logs}");
    }
}
