//! The journey evidence commands of the `fluent` binary: `fluent verify`
//! and `fluent demo record`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fluent-core/tests/fixtures")
}

fn transfer_hex() -> String {
    std::fs::read_to_string(fixtures().join("tx/transfer-preprod.hex"))
        .expect("the transfer fixture")
        .trim()
        .to_string()
}

/// Runs `fluent` with only `RUST_LOG`, feeding it `stdin`.
fn fluent(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fluent"))
        .args(args)
        .env_clear()
        .env("RUST_LOG", "trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fluent");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("run fluent")
}

fn stdout_json(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{err}: {text}"))
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn verify_exits_zero_only_on_a_match() {
    let hex = transfer_hex();
    let receiver = format!("{RECEIVER}=3000000");
    let output = fluent(
        &[
            "verify",
            "--cbor",
            &hex,
            "--expect-output",
            &receiver,
            "--expect-network",
            "preprod",
        ],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout_json(&output);
    assert_eq!(printed["verdict"], "match");
    assert_eq!(printed["tx_hash"], TRANSFER_HASH);
    assert_eq!(printed["summary"]["outputs"][0]["address"], RECEIVER);
    assert_eq!(printed["checks"].as_array().map(Vec::len), Some(2));
    assert!(stderr(&output).contains("verdict: match"));

    for (output_arg, network) in [
        (format!("{RECEIVER}=2000000"), "preprod"),
        (format!("{SENDER}=3000000"), "preprod"),
        (receiver.clone(), "mainnet"),
    ] {
        let output = fluent(
            &[
                "verify",
                "--cbor",
                &hex,
                "--expect-output",
                &output_arg,
                "--expect-network",
                network,
            ],
            "",
        );
        assert_eq!(output.status.code(), Some(1), "{output_arg} on {network}");
        let printed = stdout_json(&output);
        assert_eq!(printed["verdict"], "mismatch", "{printed}");
        assert!(stderr(&output).contains("verdict: mismatch"));
    }
}

#[test]
fn verify_reads_the_cbor_from_stdin() {
    let receiver = format!("{RECEIVER}=3000000");
    let output = fluent(
        &[
            "verify",
            "--cbor",
            "-",
            "--expect-output",
            &receiver,
            "--expect-network",
            "preprod",
        ],
        &format!("{}\n", transfer_hex()),
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout_json(&output)["verdict"], "match");
}

#[test]
fn verify_reports_bad_expectations_as_errors() {
    let hex = transfer_hex();
    let receiver = format!("{RECEIVER}=3000000");
    for (args, argument) in [
        (
            vec!["--expect-output", "nonsense", "--expect-network", "preprod"],
            "expect-output",
        ),
        (
            vec!["--expect-output", &receiver, "--expect-network", "testnet"],
            "expect-network",
        ),
        (
            vec![
                "--expect-output",
                &receiver,
                "--expect-network",
                "preprod",
                "--expect-signer",
                "xyz",
            ],
            "expect-signer",
        ),
    ] {
        let mut full = vec!["verify", "--cbor", &hex];
        full.extend(args);
        let output = fluent(&full, "");
        assert_eq!(output.status.code(), Some(1), "{argument}");
        let printed = stdout_json(&output);
        assert_eq!(printed["error"]["code"], "invalid_arguments", "{printed}");
        assert_eq!(printed["error"]["details"]["arguments"], json!([argument]));
    }
}

/// A configuration serving the transfer fixture bundle over stdio, with
/// `trp_url` as the preprod resolver.
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
async fn demo_record_writes_a_redacted_transcript() {
    let recorded = std::fs::read(fixtures().join("trp/transfer_preprod.json")).expect("envelope");
    let trp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(json!({ "method": "trp.resolve" })))
        .respond_with(ResponseTemplate::new(200).set_body_raw(recorded, "application/json"))
        .mount(&trp)
        .await;
    let dir = tempfile::tempdir().expect("temp dir");
    let config = stdio_config(dir.path(), &trp.uri());
    let out = dir.path().join("evidence");

    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "demo-record-test", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "fluent_get_skill", "arguments": {"protocol": "transfer_preprod"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
            "name": "transfer_preprod_transfer", "arguments": {
                "quantity": 3_000_000, "sender": SENDER, "receiver": RECEIVER, "middleman": SENDER}}}),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
            "name": "transfer_preprod_transfer", "arguments": {"quantity": "lots"}}}),
        json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
            "name": "sign_and_submit", "arguments": {"tx": "84a4"}}}),
    ];
    let args = [
        "demo".to_string(),
        "record".to_string(),
        "--stdio".to_string(),
        "--config".to_string(),
        config.display().to_string(),
        "--out".to_string(),
        out.display().to_string(),
    ];
    let output = tokio::task::spawn_blocking(move || {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut child = Command::new(env!("CARGO_BIN_EXE_fluent"))
            .args(&args)
            .env_clear()
            .env("RUST_LOG", "trace")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fluent demo record");
        let mut stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let mut replies = std::io::BufRead::lines(std::io::BufReader::new(stdout));
        // One message at a time, each request answered before the next, so
        // the transcript's order is the requests' order.
        for message in &messages {
            writeln!(stdin, "{message}").expect("write");
            stdin.flush().expect("flush");
            if message.get("id").is_some() {
                loop {
                    let line = replies.next().expect("a reply").expect("a line");
                    let reply: Value = serde_json::from_str(&line).expect("JSON");
                    if reply.get("id") == message.get("id") {
                        break;
                    }
                }
            }
        }
        drop(stdin);
        child.wait_with_output().expect("wait for fluent")
    })
    .await
    .expect("demo record");
    let logs = stderr(&output);
    assert!(output.status.success(), "{logs}");

    let transcripts: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("the out directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(transcripts.len(), 1, "{transcripts:?}");
    let path = &transcripts[0];
    let name = path
        .file_name()
        .expect("a file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with("transcript-") && name.ends_with("Z.md"),
        "{name}"
    );
    assert!(logs.contains(&format!("recording the transcript to {}", path.display())));
    let transcript = std::fs::read_to_string(path).expect("the transcript");

    assert!(
        transcript.starts_with("# Tx3 Fluent transcript\n"),
        "{transcript}"
    );
    assert!(transcript.contains("- Transport: stdio\n"));
    assert!(
        transcript.contains("\n## 1. `tools/list`\n"),
        "{transcript}"
    );
    assert!(transcript.contains("- Tools (6): `fluent_get_skill`, `fluent_inspect_address`, "));
    assert!(transcript.contains("\n## 2. `tools/call` · `fluent_get_skill`\n"));
    assert!(transcript.contains("\n## 3. `tools/call` · `transfer_preprod_transfer`\n"));
    assert!(transcript.contains(
        "- Arguments: `middleman`, `quantity`, `receiver`, `sender`\n- Outcome: `ok` in "
    ));
    assert!(transcript.contains(&format!("\"tx_hash\": \"{TRANSFER_HASH}\"")));
    assert!(transcript.contains("\"unsigned_tx_cbor_hex\": \"<redacted: "));
    assert!(transcript.contains("\"markdown\": \"<redacted: "));
    assert!(transcript.contains(&format!("\"address\": \"{}…", &RECEIVER[..12])));
    assert!(transcript.contains("\n## 4. `tools/call` · `transfer_preprod_transfer`\n"));
    assert!(transcript.contains("- Outcome: `invalid_arguments` in "));
    assert!(transcript.contains("- Principal: local (stdio or unauthenticated)\n"));
    assert!(transcript.contains("\n## 5. `tools/call` · `sign_and_submit`\n\n- Time: "));
    assert!(transcript.contains("- Arguments: `tx`\n- Outcome: `unknown_tool` in 0 ms\n"));

    // No argument value, CBOR or skill body is recorded, and nothing of the
    // transcript reaches the logs, even at `trace`.
    for value in [SENDER, RECEIVER, "lots", transfer_hex().as_str()] {
        assert!(!transcript.contains(value), "{value} in the transcript");
    }
    assert!(!transcript.contains("# transfer-preprod"), "a skill body");
    assert!(!logs.contains("fluent_transcript"), "{logs}");
    assert!(!logs.contains("<redacted: "), "{logs}");
}
