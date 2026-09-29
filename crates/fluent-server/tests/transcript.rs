//! The `fluent` binary's logs as `cargo xtask transcript` reads them: every
//! `tools/list` and `tools/call` answered is logged with names and outcomes
//! only, so a transcript can be rendered from the logs, and no argument
//! value or result body reaches them, even at `trace`. Transaction hashes
//! appear at `debug` only.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};
use xtask::transcript::{self, Kind};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";

/// A quantity no other part of the session contains, so finding it in the
/// logs could only mean an argument value was logged.
const QUANTITY: u64 = 3_141_593;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fluent-core/tests/fixtures")
}

/// A configuration serving the fixture bundles over stdio, with `trp_url`
/// as the preprod resolver.
fn stdio_config(dir: &Path, trp_url: &str) -> PathBuf {
    let path = dir.join("fluent.toml");
    let bundles = fixtures().join("registrations/valid");
    let text = format!(
        "[registrations]\ndir = {}\n\n[networks.preprod]\ntrp_url = \"{trp_url}\"\n\n\
         [auth]\nmode = \"none\"\n",
        toml::Value::String(bundles.display().to_string())
    );
    std::fs::write(&path, text).expect("write configuration");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn the_logs_render_a_transcript_without_argument_values() {
    let recorded = std::fs::read(fixtures().join("trp/transfer_preprod.json")).expect("envelope");
    let trp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "trp.resolve" })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(recorded, "application/json"))
        .mount(&trp)
        .await;
    let dir = tempfile::tempdir().expect("temp dir");
    let config = stdio_config(dir.path(), &trp.uri());

    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "transcript-test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "fluent_get_skill", "arguments": {"protocol": "transfer_preprod"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "fluent_inspect_address", "arguments": {"address": RECEIVER}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
            "name": "transfer_preprod_transfer", "arguments": {
                "quantity": QUANTITY, "sender": SENDER, "receiver": RECEIVER, "middleman": SENDER}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
            "name": "transfer_preprod_transfer", "arguments": {"quantity": "lots"}}}),
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
            "name": "sign_and_submit", "arguments": {"tx": "84a4deadbeef"}}}),
    ];
    let config_arg = config.display().to_string();
    let (output, replies) = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fluent"))
            .args(["serve", "--stdio", "--config", &config_arg])
            .env_clear()
            .env("RUST_LOG", "trace")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fluent serve");
        let mut stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let mut lines = BufReader::new(stdout).lines();
        let mut replies = Vec::new();
        // One message at a time, each request answered before the next, so
        // the log's order is the requests' order.
        for message in &messages {
            writeln!(stdin, "{message}").expect("write");
            stdin.flush().expect("flush");
            if message.get("id").is_some() {
                loop {
                    let line = lines.next().expect("a reply").expect("a line");
                    let reply: Value = serde_json::from_str(&line).expect("JSON");
                    if reply.get("id") == message.get("id") {
                        replies.push(reply);
                        break;
                    }
                }
            }
        }
        drop(stdin);
        (child.wait_with_output().expect("wait for fluent"), replies)
    })
    .await
    .expect("fluent serve");
    let logs = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{logs}");

    // The session did prepare the transfer, so its values were in play.
    let prepared = &replies[4]["result"]["structuredContent"];
    assert_eq!(prepared["tx_hash"], TRANSFER_HASH, "{:#}", replies[4]);
    let cbor = prepared["unsigned_tx_cbor_hex"].as_str().expect("the CBOR");

    // No argument value or result body reaches the logs, even at `trace`.
    for value in [
        SENDER,
        RECEIVER,
        &QUANTITY.to_string(),
        "lots",
        "84a4deadbeef",
        &cbor[..64],
        "# transfer",
    ] {
        assert!(!logs.contains(value), "{value} in the logs:\n{logs}");
    }
    // The transaction hash is logged at `debug` only, never at `info`.
    for line in logs.lines().filter(|line| line.contains(TRANSFER_HASH)) {
        let line: Value = serde_json::from_str(line).expect("a JSON log line");
        assert_eq!(line["level"], "DEBUG", "{line}");
    }

    let read = transcript::read(&logs);
    let kinds: Vec<(&str, &str)> = read
        .entries
        .iter()
        .map(|entry| match &entry.kind {
            Kind::List { .. } => ("tools/list", ""),
            Kind::Call { tool, outcome, .. } => (tool.as_str(), outcome.as_str()),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("tools/list", ""),
            ("fluent_get_skill", "ok"),
            ("fluent_inspect_address", "ok"),
            ("transfer_preprod_transfer", "ok"),
            ("transfer_preprod_transfer", "invalid_arguments"),
            ("sign_and_submit", "unknown_tool"),
        ],
        "{logs}"
    );
    assert!(read.entries.iter().all(|entry| entry.sub_hash.is_none()));

    let text = read.to_markdown("`fluent serve --stdio`");
    assert!(text.contains("- Principals: local (stdio or unauthenticated) (6)\n"));
    assert!(text.contains("- Tools (6): `fluent_get_skill`, `fluent_inspect_address`, "));
    assert!(text.contains("\n## 4. `tools/call` · `transfer_preprod_transfer`\n\n- Time: "));
    assert!(text.contains(
        "- Transaction: `transfer` of `transfer_preprod`\n\
         - Arguments: `middleman`, `quantity`, `receiver`, `sender`\n\
         - Outcome: `ok` in "
    ));
    assert!(text.contains("- Outcome: `invalid_arguments` in "));
    assert!(text.contains("\n## 6. `tools/call` · `sign_and_submit`\n\n- Time: "));
    assert!(text.contains("- Arguments: `tx`\n- Outcome: `unknown_tool` in 0 ms\n"));
}
