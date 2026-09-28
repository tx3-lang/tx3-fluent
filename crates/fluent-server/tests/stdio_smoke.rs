//! End-to-end smoke test: launch `fluent serve --stdio` over the transfer and
//! Strike fixture bundles, drive it with MCP JSON-RPC lines, and check the
//! listing and each kind of tool call against a scripted TRP resolver.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";

/// How long one response may take before the test fails instead of hanging.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fluent-core/tests/fixtures")
}

/// Writes a configuration serving the valid fixture bundles, with `trp_url`
/// as the preprod resolver.
fn config(name: &str, trp_url: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.toml"));
    let dir = fixtures().join("registrations/valid");
    let text = format!(
        "[registrations]\ndir = {}\n\n[networks.preprod]\ntrp_url = \"{trp_url}\"\n\n\
         [auth]\nmode = \"none\"\n",
        toml::Value::String(dir.display().to_string())
    );
    std::fs::write(&path, text).expect("write configuration");
    path
}

/// A running `fluent serve --stdio`, answering one JSON-RPC line at a time.
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Session {
    fn start(config: &Path) -> Session {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fluent"))
            .args(["serve", "--stdio", "--config"])
            .arg(config)
            .env_clear()
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn fluent serve --stdio");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Session {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write to fluent");
        self.stdin.flush().expect("flush to fluent");
    }

    /// Sends a request and returns its response, skipping notifications.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(RESPONSE_TIMEOUT)
                .unwrap_or_else(|err| panic!("no response to {method}: {err}"));
            if line.trim().is_empty() {
                continue;
            }
            let message: Value =
                serde_json::from_str(&line).unwrap_or_else(|err| panic!("{err}: {line}"));
            if message["id"] == id {
                return message;
            }
        }
    }

    fn initialize(&mut self) -> Value {
        let response = self.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "stdio-smoke", "version": "0" }
            }),
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        response
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let response = self.request(
            id,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{tool} answered without a result: {response}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The JSON of a tool result's single text content.
fn text_json(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {result}"));
    serde_json::from_str(text).unwrap_or_else(|err| panic!("{err}: {text}"))
}

fn contains_ref(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => map.contains_key("$ref") || map.values().any(contains_ref),
        Value::Array(items) => items.iter().any(contains_ref),
        _ => false,
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

#[tokio::test(flavor = "multi_thread")]
async fn serves_the_catalog_over_stdio() {
    let tx_hex = std::fs::read_to_string(fixtures().join("tx/transfer-preprod.hex")).unwrap();
    let server = resolver_answering(json!({
        "jsonrpc": "2.0",
        "id": "1",
        "result": { "hash": TRANSFER_HASH, "tx": tx_hex.trim() }
    }))
    .await;
    let config = config("stdio-smoke", &server.uri());

    tokio::task::spawn_blocking(move || {
        let mut session = Session::start(&config);

        let init = session.initialize();
        let info = &init["result"];
        assert_eq!(info["serverInfo"]["name"], "tx3-fluent", "{init}");
        assert_eq!(info["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(info["capabilities"]["tools"]["listChanged"], true);
        let instructions = info["instructions"].as_str().expect("instructions");
        assert!(instructions.starts_with("Tx3 Fluent prepares UNSIGNED"));

        let list = session.request(2, "tools/list", json!({}));
        let tools = list["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "fluent_get_skill",
                "fluent_inspect_address",
                "strike_staking_mainnet_add_stake",
                "strike_staking_mainnet_stake",
                "strike_staking_mainnet_withdraw_stake",
                "transfer_preprod_transfer",
            ]
        );
        for tool in tools {
            assert!(!contains_ref(&tool["inputSchema"]), "$ref in {tool}");
            assert!(!contains_ref(&tool["outputSchema"]), "$ref in {tool}");
            assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
        }

        let skill = session.call(
            3,
            "fluent_get_skill",
            json!({ "protocol": "strike-finance/strike-staking" }),
        );
        assert_eq!(skill["isError"], false, "{skill}");
        let content = &skill["structuredContent"];
        assert_eq!(content, &text_json(&skill));
        assert_eq!(
            content["protocol"]["registration_slug"],
            "strike_staking_mainnet"
        );
        assert_eq!(content["protocol"]["network"], "mainnet");
        assert_eq!(content["skill_revision"], 1);
        assert_eq!(content["dependencies"][0]["id"], "strike_balance");
        assert!(
            content["markdown"]
                .as_str()
                .unwrap()
                .contains("# strike-staking-mainnet")
        );

        let missing = session.call(4, "fluent_get_skill", json!({ "protocol": "acme/nothing" }));
        assert_eq!(missing["isError"], true, "{missing}");
        assert_eq!(text_json(&missing)["error"]["code"], "unknown_protocol");

        let address = session.call(5, "fluent_inspect_address", json!({ "address": RECEIVER }));
        assert_eq!(address["isError"], false, "{address}");
        let report = &address["structuredContent"];
        assert_eq!(report, &text_json(&address));
        assert_eq!(report["kind"], "shelley_base");
        assert_eq!(report["network"], "preprod_or_preview");

        let prepared = session.call(6, "transfer_preprod_transfer", transfer_args());
        assert_eq!(prepared["isError"], false, "{prepared}");
        let envelope = &prepared["structuredContent"];
        assert_eq!(envelope, &text_json(&prepared));
        assert_eq!(envelope["status"], "prepared_unsigned");
        assert_eq!(envelope["signed"], false);
        assert_eq!(envelope["submitted"], false);
        assert_eq!(envelope["tx_hash"], TRANSFER_HASH);
        assert_eq!(
            envelope["protocol"]["registration_slug"],
            "transfer_preprod"
        );
        assert_eq!(envelope["summary"]["outputs"][0]["address"], RECEIVER);
        assert_eq!(envelope["summary"]["outputs"][0]["lovelace"], 3_000_000);

        let invalid = session.call(
            7,
            "transfer_preprod_transfer",
            json!({ "quantity": "lots" }),
        );
        assert_eq!(invalid["isError"], true, "{invalid}");
        assert_eq!(text_json(&invalid)["error"]["code"], "invalid_arguments");

        let unknown = session.request(
            8,
            "tools/call",
            json!({ "name": "nothing_here", "arguments": {} }),
        );
        assert!(unknown["error"]["code"].is_i64(), "{unknown}");
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_failures_are_tool_errors() {
    let server = resolver_answering(json!({
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
                    "minAmount": { "lovelace": "8000000" },
                    "refs": [],
                    "supportMany": true
                },
                "search_space": { "matched": [], "byAddressCount": 0 }
            }
        }
    }))
    .await;
    let config = config("stdio-smoke-insufficient", &server.uri());

    tokio::task::spawn_blocking(move || {
        let mut session = Session::start(&config);
        session.initialize();

        let result = session.call(2, "transfer_preprod_transfer", transfer_args());
        assert_eq!(result["isError"], true, "{result}");
        assert!(result.get("structuredContent").is_none(), "{result}");
        let error = &text_json(&result)["error"];
        assert_eq!(error["code"], "insufficient_funds");
        assert_eq!(error["details"]["input"], "source");
        assert!(
            !result.to_string().contains(SENDER),
            "leaked the sender: {result}"
        );
    })
    .await
    .unwrap();
}
