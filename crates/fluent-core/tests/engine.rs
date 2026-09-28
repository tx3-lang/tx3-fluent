//! `Engine::prepare` against a scripted TRP endpoint.
//!
//! A `wiremock` server stands in for the resolver: it answers `trp.resolve`
//! with a success envelope or with each JSON-RPC error the TRP specification
//! defines, and the tests check the engine's classification, one test per
//! row of the mapping in `fluent_core::engine::resolver`. The success
//! envelope carries the real preprod transfer in
//! `tests/fixtures/tx/transfer-preprod.hex`.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use fluent_core::engine::API_KEY_HEADER;
use fluent_core::engine::resolver::SCRIPT_LOG_LINES;
use fluent_core::registration::{Catalog, load_dir};
use fluent_core::{Config, Engine, ErrorCode, FluentError, PrepareRequest, PreparedTransaction};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";
const API_KEY: &str = "sentinel-trp-key-3c1d";

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn transfer_hex() -> String {
    fs::read_to_string(fixture("tx/transfer-preprod.hex"))
        .expect("the transfer fixture is readable")
        .trim()
        .to_string()
}

/// The valid fixture registrations: `transfer_preprod` and
/// `strike_staking_mainnet`.
fn catalog() -> Catalog {
    let loaded = load_dir(fixture("registrations/valid")).expect("the fixtures load");
    assert!(loaded.rejected.is_empty(), "{:?}", loaded.rejected);
    loaded.catalog
}

/// A configuration serving only preprod, at `trp_url`, with the API key set
/// when `api_key` is.
fn config(trp_url: &str, api_key: Option<&str>) -> Config {
    let text = format!(
        r#"
        [registrations]
        dir = "unused"

        [networks.preprod]
        trp_url = "{trp_url}"
        trp_api_key_env = "TEST_TRP_API_KEY"

        [limits]
        resolver_timeout_secs = 1
        global_cutoff_secs = 1

        [auth]
        mode = "none"
        "#
    );
    let env: Vec<(&str, &str)> = api_key
        .map(|key| ("TEST_TRP_API_KEY", key))
        .into_iter()
        .collect();
    Config::from_toml_str(&text, env).expect("a valid test configuration")
}

fn engine(server: &MockServer) -> Engine {
    Engine::new(&config(&server.uri(), Some(API_KEY)), &catalog())
}

fn transfer(args: Value) -> PrepareRequest {
    PrepareRequest {
        registration: "transfer_preprod".into(),
        tx: "transfer".into(),
        args,
    }
}

fn transfer_args() -> Value {
    json!({
        "quantity": 3_000_000,
        "sender": SENDER,
        "receiver": RECEIVER,
        "middleman": SENDER
    })
}

async fn resolver_answering(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "trp.resolve" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

async fn resolver_failing(code: i32, message: &str, data: Value) -> MockServer {
    resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "error": { "code": code, "message": message, "data": data }
    }))
    .await
}

async fn prepare_against(server: &MockServer) -> Result<PreparedTransaction, FluentError> {
    engine(server).prepare(transfer(transfer_args())).await
}

async fn fails_with(server: &MockServer, code: ErrorCode) -> FluentError {
    let err = prepare_against(server)
        .await
        .expect_err("the prepare should fail");
    assert_eq!(err.code(), code, "{err:?}");
    err
}

async fn requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap_or_default()
}

fn input_not_resolved(min_amount: Value) -> Value {
    json!({
        "name": "source",
        "query": {
            "address": SENDER,
            "collateral": false,
            "minAmount": min_amount,
            "refs": [],
            "supportMany": true
        },
        "search_space": { "matched": [], "byAddressCount": 0 }
    })
}

#[tokio::test]
async fn prepares_a_transfer_from_the_resolver_envelope() {
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": TRANSFER_HASH, "tx": transfer_hex() }
    }))
    .await;

    let prepared = prepare_against(&server).await.unwrap();
    let registration = catalog().get("transfer_preprod").unwrap().clone();
    assert_eq!(prepared.protocol.scope, "unknown");
    assert_eq!(prepared.protocol.name, "unknown");
    assert_eq!(prepared.protocol.version, "0.0.1");
    assert_eq!(prepared.protocol.registration_slug, "transfer_preprod");
    assert_eq!(
        prepared.protocol.registration_revision,
        registration.revision()
    );
    assert_eq!(prepared.protocol.tii_digest, registration.tii_digest());
    assert_eq!(prepared.transaction, "transfer");
    assert_eq!(prepared.network, "preprod");
    assert_eq!(prepared.tx_hash, TRANSFER_HASH);
    assert_eq!(prepared.unsigned_tx_cbor_hex, transfer_hex());
    let expected = fluent_core::summary::decode(&transfer_hex()).unwrap();
    assert_eq!(prepared.summary, expected.transaction);
    assert_eq!(prepared.summary.outputs[0].address, RECEIVER);

    let envelope = serde_json::to_value(&prepared).unwrap();
    assert_eq!(envelope["status"], "prepared_unsigned");
    assert_eq!(envelope["signed"], false);
    assert_eq!(envelope["submitted"], false);
}

#[tokio::test]
async fn sends_caller_arguments_parties_and_profile_values() {
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": TRANSFER_HASH, "tx": transfer_hex() }
    }))
    .await;
    prepare_against(&server).await.unwrap();

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]
            .headers
            .get(API_KEY_HEADER)
            .map(|value| value.to_str().unwrap()),
        Some(API_KEY)
    );
    let body: Value = sent[0].body_json().unwrap();
    assert_eq!(body["method"], "trp.resolve");
    assert_eq!(
        body["params"]["args"],
        json!({
            "quantity": 3_000_000,
            "sender": SENDER,
            "receiver": RECEIVER,
            "middleman": SENDER,
            // Bound by the preprod profile, sent by the SDK.
            "tax": 5_000_000
        })
    );
    assert_eq!(body["params"]["tir"]["version"], "v1beta0");
}

#[tokio::test]
async fn omits_the_api_key_header_when_its_variable_is_unset() {
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": TRANSFER_HASH, "tx": transfer_hex() }
    }))
    .await;
    let engine = Engine::new(&config(&server.uri(), None), &catalog());
    engine.prepare(transfer(transfer_args())).await.unwrap();
    let sent = requests(&server).await;
    assert!(sent[0].headers.get(API_KEY_HEADER).is_none());
}

#[tokio::test]
async fn input_not_resolved_with_a_min_amount_is_insufficient_funds() {
    let server = resolver_failing(
        -32002,
        "Input not resolved",
        input_not_resolved(json!({ "lovelace": "8000000" })),
    )
    .await;
    let err = fails_with(&server, ErrorCode::InsufficientFunds).await;
    assert_eq!(
        err.details(),
        Some(json!({
            "input": "source",
            "query": { "has_address": true, "refs": 0 },
            "search_space": { "matched": 0 }
        }))
    );
}

#[tokio::test]
async fn input_not_resolved_without_a_min_amount_is_input_not_resolved() {
    let server =
        resolver_failing(-32002, "Input not resolved", input_not_resolved(json!({}))).await;
    let err = fails_with(&server, ErrorCode::InputNotResolved).await;
    assert_eq!(err.details().unwrap()["input"], "source");
}

#[tokio::test]
async fn script_failures_keep_twenty_log_lines() {
    let logs: Vec<String> = (1..=30).map(|n| format!("trace line {n}")).collect();
    let server = resolver_failing(-32003, "Tx script failure", json!({ "logs": logs })).await;
    let err = fails_with(&server, ErrorCode::ScriptFailure).await;
    assert_eq!(
        err.details(),
        Some(json!({ "logs": logs[..SCRIPT_LOG_LINES] }))
    );
}

#[tokio::test]
async fn missing_arguments_are_invalid_arguments() {
    let server = resolver_failing(
        -32001,
        "Missing transaction argument",
        json!({ "key": "quantity", "type": "Int" }),
    )
    .await;
    let err = fails_with(&server, ErrorCode::InvalidArguments).await;
    assert_eq!(err.details().unwrap()["arguments"], json!(["quantity"]));
}

#[tokio::test]
async fn unsupported_tir_is_registration_unavailable() {
    let server = resolver_failing(
        -32000,
        "Unsupported TIR",
        json!({ "expected": "v1beta1", "provided": "v1beta0" }),
    )
    .await;
    let err = fails_with(&server, ErrorCode::RegistrationUnavailable).await;
    assert_eq!(
        err.message(),
        "registration transfer_preprod is unavailable: the resolver does not support TIR \
         version v1beta0; it expects v1beta1"
    );
}

#[tokio::test]
async fn http_errors_are_resolver_unavailable_with_the_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream down"))
        .mount(&server)
        .await;
    let err = fails_with(&server, ErrorCode::ResolverUnavailable).await;
    assert_eq!(
        err.details(),
        Some(json!({ "network": "preprod", "status": 503 }))
    );
}

#[tokio::test]
async fn unreachable_resolvers_are_resolver_unavailable() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let engine = Engine::new(&config(&url, None), &catalog());
    let err = engine.prepare(transfer(transfer_args())).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ResolverUnavailable);
    assert_eq!(err.details(), Some(json!({ "network": "preprod" })));
}

#[tokio::test]
async fn unclassified_rpc_errors_are_resolver_unavailable() {
    let server = resolver_failing(-32603, "Internal error", json!(null)).await;
    let err = fails_with(&server, ErrorCode::ResolverUnavailable).await;
    assert_eq!(err.details(), Some(json!({ "network": "preprod" })));
}

#[tokio::test]
async fn slow_resolvers_time_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": "1",
                    "result": { "hash": TRANSFER_HASH, "tx": transfer_hex() }
                })),
        )
        .mount(&server)
        .await;
    let err = fails_with(&server, ErrorCode::ResolverTimeout).await;
    assert_eq!(err.details(), Some(json!({ "timeout_secs": 1 })));
}

#[tokio::test]
async fn unknown_transactions_are_rejected_before_resolving() {
    let server = MockServer::start().await;
    let err = engine(&server)
        .prepare(PrepareRequest {
            registration: "transfer_preprod".into(),
            tx: "swap".into(),
            args: transfer_args(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownTransaction);
    assert_eq!(
        err.details(),
        Some(json!({ "protocol": "transfer_preprod", "transaction": "swap" }))
    );
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn a_hash_that_differs_from_the_body_is_internal() {
    let wrong = "00".repeat(32);
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": wrong, "tx": transfer_hex() }
    }))
    .await;
    let err = fails_with(&server, ErrorCode::Internal).await;
    assert_eq!(err.message(), "internal error");
    assert_eq!(
        err.details(),
        Some(json!({ "resolver_tx_hash": wrong, "computed_tx_hash": TRANSFER_HASH }))
    );
}

#[tokio::test]
async fn undecodable_cbor_is_internal() {
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": TRANSFER_HASH, "tx": "84a0" }
    }))
    .await;
    fails_with(&server, ErrorCode::Internal).await;
}

#[tokio::test]
async fn invalid_arguments_never_reach_the_resolver() {
    let server = MockServer::start().await;
    let engine = engine(&server);
    let cases = [
        ("tax", json!(1)),
        ("TAX", json!(1)),
        ("quantity", json!("three")),
        ("unexpected", json!(true)),
    ];
    for (key, value) in cases {
        let mut args = transfer_args();
        args[key] = value;
        let err = engine.prepare(transfer(args)).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidArguments, "{key}");
    }
    let mut args = transfer_args();
    args.as_object_mut().unwrap().remove("receiver");
    let err = engine.prepare(transfer(args)).await.unwrap_err();
    assert_eq!(err.details().unwrap()["arguments"], json!(["receiver"]));

    let mut args = transfer_args();
    args["tax"] = json!(0);
    let err = engine.prepare(transfer(args)).await.unwrap_err();
    assert_eq!(
        err.message(),
        "invalid arguments: deployment-bound value cannot be overridden"
    );
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn unknown_registrations_are_unknown_protocols() {
    let server = MockServer::start().await;
    let err = engine(&server)
        .prepare(PrepareRequest {
            registration: "nothing_here".into(),
            tx: "transfer".into(),
            args: transfer_args(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::UnknownProtocol);
}

#[tokio::test]
async fn registrations_on_unserved_networks_are_network_mismatches() {
    let server = MockServer::start().await;
    let err = engine(&server)
        .prepare(PrepareRequest {
            registration: "strike_staking_mainnet".into(),
            tx: "stake".into(),
            args: json!({}),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NetworkMismatch);
    assert_eq!(
        err.details(),
        Some(json!({ "requested": "mainnet", "available": ["preprod"] }))
    );
}
