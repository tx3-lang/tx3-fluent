//! The `cargo xtask` commands: `verify` and `transcript`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";
const TRANSFER_HASH: &str = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/fluent-core/tests/fixtures")
}

fn transfer_hex() -> String {
    std::fs::read_to_string(fixtures().join("tx/transfer-preprod.hex"))
        .expect("the transfer fixture")
        .trim()
        .to_string()
}

/// Runs `xtask` with an empty environment, feeding it `stdin`.
fn xtask(args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xtask");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("run xtask")
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
    let output = xtask(
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
        let output = xtask(
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
    let output = xtask(
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
        let output = xtask(&full, "");
        assert_eq!(output.status.code(), Some(1), "{argument}");
        let printed = stdout_json(&output);
        assert_eq!(printed["error"]["code"], "invalid_arguments", "{printed}");
        assert_eq!(printed["error"]["details"]["arguments"], json!([argument]));
    }
}

/// `fixtures/serve-stdio.log`: the stderr of `fluent serve --stdio` at
/// `info`, captured unedited, for a `tools/list`, a `fluent_inspect_address`
/// call with a receiver address, and a `sign_and_submit` call the server does
/// not have. The unknown call was answered first.
fn captured_logs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serve-stdio.log")
}

#[test]
fn transcript_renders_captured_logs() {
    let logs = captured_logs();
    let logs = logs.to_str().expect("a UTF-8 path");
    let output = xtask(&["transcript", "--logs", logs], "");
    assert!(output.status.success(), "{}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.starts_with(&format!(
            "# Tx3 Fluent transcript\n\n\
             - Rendered by: `cargo xtask transcript` from `{logs}`\n\
             - Log lines read: 12\n\
             - Entries: 3 (1 `tools/list`, 2 `tools/call`)\n\
             - From 2026-09-29T23:16:47.866382Z to 2026-09-29T23:16:47.866698Z\n\
             - Principals: local (stdio or unauthenticated) (3)\n"
        )),
        "{text}"
    );
    assert!(text.contains(
        "\n## 1. `tools/list`\n\n\
         - Time: 2026-09-29T23:16:47.866382Z\n\
         - Principal: local (stdio or unauthenticated)\n\
         - Tools (6): `fluent_get_skill`, `fluent_inspect_address`, \
         `strike_staking_mainnet_add_stake`, `strike_staking_mainnet_stake`, \
         `strike_staking_mainnet_withdraw_stake`, `transfer_preprod_transfer`\n"
    ));
    assert!(text.contains(
        "\n## 2. `tools/call` · `sign_and_submit`\n\n\
         - Time: 2026-09-29T23:16:47.866445Z\n\
         - Principal: local (stdio or unauthenticated)\n\
         - Arguments: `tx`\n\
         - Outcome: `unknown_tool` in 0 ms\n"
    ));
    assert!(text.ends_with(
        "\n## 3. `tools/call` · `fluent_inspect_address`\n\n\
         - Time: 2026-09-29T23:16:47.866698Z\n\
         - Principal: local (stdio or unauthenticated)\n\
         - Arguments: `address`\n\
         - Outcome: `ok` in 0 ms\n"
    ));
    assert!(!text.contains("addr_test1"), "an argument value: {text}");
    assert!(stderr(&output).contains("3 entries from 12 log lines"));
}

#[test]
fn transcript_reads_stdin_writes_out_and_filters_by_principal() {
    let logs = std::fs::read_to_string(captured_logs()).expect("the captured logs");
    let dir = std::env::temp_dir().join(format!("xtask-transcript-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = dir.join("transcript.md");
    let out_arg = out.to_str().expect("a UTF-8 path");

    let output = xtask(&["transcript", "--out", out_arg], &logs);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(output.stdout.is_empty());
    let text = std::fs::read_to_string(&out).expect("the transcript");
    assert!(text.contains("from stdin\n"), "{text}");
    assert!(text.contains("- Entries: 3 "), "{text}");
    assert!(stderr(&output).contains(&format!("written to {out_arg}")));

    // Over stdio there is no principal, so a hash filter keeps nothing.
    let output = xtask(&["transcript", "--sub-hash", "0123456789ab"], &logs);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("- Entries: 0 (0 `tools/list`, 0 `tools/call`)\n"),
        "{text}"
    );
    assert!(!text.contains("\n## "), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
