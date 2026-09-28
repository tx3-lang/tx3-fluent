//! End-to-end checks of the `fluent` binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fluent_core::registration::sha256_digest;

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/config")
        .join(name)
}

/// Runs `fluent` with only the given environment, so the caller's variables
/// cannot leak in as overrides.
fn fluent(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fluent"))
        .args(args)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("run fluent")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn prints_its_version() {
    let output = fluent(&["--version"], &[]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output).trim(),
        format!("fluent {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn config_check_prints_the_redacted_configuration() {
    let secrets = [
        ("TRP_MAINNET_API_KEY", "sentinel-mainnet-6f2a"),
        ("TRP_PREPROD_API_KEY", "sentinel-preprod-91bd"),
        ("FLUENT_SESSION_SECRET", "sentinel-session-0c4e"),
        ("FLUENT_OIDC_CLIENT_ID", "sentinel-client-id-7a13"),
        ("FLUENT_OIDC_CLIENT_SECRET", "sentinel-client-secret-e58d"),
    ];
    let path = example("hosted.toml");
    let output = fluent(
        &["config", "check", "--config", path.to_str().unwrap()],
        &secrets,
    );
    assert!(output.status.success(), "{}", stderr(&output));

    let printed = stdout(&output);
    assert!(printed.contains("[networks.mainnet]"), "{printed}");
    assert!(printed.contains("mode = \"oidc\""), "{printed}");
    assert!(printed.contains("\"<set>\""), "{printed}");
    for (var, value) in secrets {
        assert!(!printed.contains(value), "leaked {var}: {printed}");
        assert!(!stderr(&output).contains(value), "leaked {var} to stderr");
    }
}

#[test]
fn config_check_applies_environment_overrides() {
    let path = example("self-hosted.toml");
    let output = fluent(
        &["config", "check", "--config", path.to_str().unwrap()],
        &[("FLUENT_SERVER__LISTEN", "127.0.0.1:9999")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("listen = \"127.0.0.1:9999\""),
        "{}",
        stdout(&output)
    );
}

#[test]
fn config_check_rejects_unknown_keys() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join("unknown-key.toml");
    let text = std::fs::read_to_string(example("self-hosted.toml")).unwrap();
    std::fs::write(&path, format!("{text}\n[auth_extra]\nmode = \"none\"\n")).unwrap();

    let output = fluent(
        &["config", "check", "--config", path.to_str().unwrap()],
        &[],
    );
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("unknown field"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn config_check_rejects_an_auth_mode_override_that_would_drop_keys() {
    let path = example("self-hosted.toml");
    let output = fluent(
        &["config", "check", "--config", path.to_str().unwrap()],
        &[("FLUENT_AUTH__MODE", "none")],
    );
    assert!(!output.status.success(), "{}", stdout(&output));
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("unknown field `token_env`"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn config_check_reports_a_missing_file() {
    let output = fluent(&["config", "check", "--config", "does-not-exist.toml"], &[]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("does-not-exist.toml"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn address_inspect_prints_the_report_as_json() {
    let address = "addr_test1vz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzerspjrlsz";
    let output = fluent(&["address", "inspect", address], &[]);
    assert!(output.status.success(), "{}", stderr(&output));

    let report: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(report["input"], address);
    assert_eq!(report["kind"], "shelley_enterprise");
    assert_eq!(report["network"], "preprod_or_preview");
    assert_eq!(report["network_id"], 0);
    assert_eq!(report["payment_credential"]["type"], "key_hash");
    assert_eq!(
        report["payment_credential"]["hash_hex"],
        "9493315cd92eb5d8c4304e67b7e16ae36d61d34502694657811a2c8e"
    );
    assert_eq!(report["stake_credential"], serde_json::Value::Null);
}

#[test]
fn address_inspect_rejects_malformed_input() {
    let output = fluent(&["address", "inspect", "addr1notarealaddress"], &[]);
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty());
    assert!(
        stderr(&output).contains("invalid arguments: address is not valid bech32"),
        "{}",
        stderr(&output)
    );
}

fn registrations(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../fluent-core/tests/fixtures/registrations")
        .join(dir)
}

/// Writes a minimal configuration reading registrations from `dir`.
fn config_for(name: &str, dir: &Path) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.toml"));
    let text = format!(
        "[registrations]\ndir = {}\n\n[networks.preprod]\ntrp_url = \"https://trp.example\"\n\n\
         [auth]\nmode = \"none\"\n",
        toml::Value::String(dir.display().to_string())
    );
    std::fs::write(&path, text).expect("write configuration");
    path
}

fn check_registrations(config: &Path) -> (Output, toml::Table) {
    let config = config.to_str().expect("a UTF-8 path");
    let output = fluent(&["registrations", "check", "--config", config], &[]);
    let report =
        toml::from_str(&stdout(&output)).unwrap_or_else(|err| panic!("{err}: {}", stdout(&output)));
    (output, report)
}

fn rows<'a>(report: &'a toml::Table, key: &str) -> Vec<&'a toml::Table> {
    report[key]
        .as_array()
        .expect("an array of tables")
        .iter()
        .map(|row| row.as_table().expect("a table"))
        .collect()
}

#[test]
fn registrations_check_prints_each_registration() {
    let dir = registrations("valid");
    let (output, report) = check_registrations(&config_for("registrations-valid", &dir));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!report.contains_key("rejected"), "{report}");

    // Digests and revisions computed with sha256sum; see
    // crates/fluent-core/tests/registrations.rs.
    let expected = [
        [
            ("slug", "strike_staking_mainnet"),
            ("protocol", "strike-finance/strike-staking:0.1.0"),
            ("network", "mainnet"),
            ("profile", "mainnet"),
            ("revision", "ec7797deaa59"),
            (
                "tii_digest",
                "sha256:8da7d6658f6083325a644b7e1c6af6dc57494a7baf1addd7ed624499a2064bd3",
            ),
            (
                "skill_digest",
                "sha256:17744829d0f6f415d6c3f5a433d6568987016508edc6a0f170145a3779f128ae",
            ),
        ],
        [
            ("slug", "transfer_preprod"),
            ("protocol", "unknown/unknown:0.0.1"),
            ("network", "preprod"),
            ("profile", "preprod"),
            ("revision", "04948dcd8aa4"),
            (
                "tii_digest",
                "sha256:8d5d715f300e618373b588af96f0cfb9b0a3904b3a34fc0038f72ebb1695da31",
            ),
            (
                "skill_digest",
                "sha256:95afa7141036ade51321df3e8f203727240f0bebd14fd583c0a7be7d56fbe0cd",
            ),
        ],
    ];
    let printed = rows(&report, "registration");
    assert_eq!(printed.len(), expected.len(), "{report}");
    for (row, fields) in printed.into_iter().zip(expected) {
        for (key, value) in fields {
            assert_eq!(row[key].as_str(), Some(value), "{key} in {row}");
        }
        let slug = row["slug"].as_str().unwrap();
        assert_eq!(
            row["bundle"].as_str(),
            Some(dir.join(slug).display().to_string().as_str())
        );
        // Local bundles carry no registry provenance.
        assert!(!row.contains_key("provenance"), "{row}");
    }
}

#[test]
fn registrations_check_prints_rejections_and_fails() {
    let dir = registrations("invalid");
    let (output, report) = check_registrations(&config_for("registrations-invalid", &dir));
    assert!(!output.status.success());
    assert!(!report.contains_key("registration"), "{report}");
    assert!(
        stderr(&output).contains("5 of 5 registration bundles rejected"),
        "{}",
        stderr(&output)
    );

    let expected = [
        ("local_profile", "registration_unavailable"),
        ("malformed_tii", "registration_unavailable"),
        ("missing_profile", "registration_unavailable"),
        ("wrong_network", "network_mismatch"),
        ("wrong_skill_binding", "registration_unavailable"),
    ];
    let printed = rows(&report, "rejected");
    assert_eq!(printed.len(), expected.len(), "{report}");
    for (row, (name, code)) in printed.into_iter().zip(expected) {
        let bundle = dir.join(name).display().to_string();
        assert_eq!(row["bundle"].as_str(), Some(bundle.as_str()));
        assert_eq!(row["code"].as_str(), Some(code), "{row}");
        assert!(row["message"].as_str().unwrap().contains(&bundle), "{row}");
    }
}

#[test]
fn registrations_check_reports_an_unreadable_directory() {
    let dir = registrations("does-not-exist");
    let config = config_for("registrations-missing", &dir);
    let output = fluent(
        &[
            "registrations",
            "check",
            "--config",
            config.to_str().unwrap(),
        ],
        &[],
    );
    assert!(!output.status.success());
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    let err = stderr(&output);
    assert!(err.contains(&dir.display().to_string()), "{err}");
    assert!(
        err.contains("cannot read the registrations directory"),
        "{err}"
    );
}

#[test]
fn registrations_check_prints_registry_provenance() {
    // A registry bundle whose artifact is already in a verified cache, so the
    // check makes no request; nothing listens on the registry URL.
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("registrations-registry");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    let transfer = registrations("valid/transfer_preprod");
    let tii = std::fs::read(transfer.join("protocol.tii")).unwrap();
    let source = b"tx transfer() {}\n";
    let layer = |media_type: &str, bytes: &[u8]| {
        serde_json::json!({
            "mediaType": media_type,
            "digest": sha256_digest(bytes),
            "size": bytes.len(),
        })
    };
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": sha256_digest(b"{}"),
            "size": 2,
        },
        "layers": [layer("application/tx3", source), layer("application/tii+json", &tii)],
        "annotations": { "org.opencontainers.image.revision": "6bcfa90f" },
    }))
    .unwrap();
    let digest = sha256_digest(&manifest);
    let cache = dir.join(".cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(format!("{digest}.manifest.json")), &manifest).unwrap();
    std::fs::write(cache.join(format!("{digest}.tii")), &tii).unwrap();

    let bundle = dir.join("transfer");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::copy(transfer.join("SKILL.md"), bundle.join("SKILL.md")).unwrap();
    let text = std::fs::read_to_string(transfer.join("registration.toml")).unwrap();
    let text = text.replacen(
        "source = \"local\"",
        &format!(
            "source = \"registry\"\n\n[protocol.registry]\nurl = \"http://127.0.0.1:9\"\n\
             ref = \"unknown/unknown:0.0.1\"\nmanifest_digest = \"{digest}\""
        ),
        1,
    );
    std::fs::write(bundle.join("registration.toml"), text).unwrap();

    let (output, report) = check_registrations(&config_for("registrations-registry", &dir));
    assert!(output.status.success(), "{}", stderr(&output));
    let printed = rows(&report, "registration");
    assert_eq!(printed.len(), 1, "{report}");
    assert_eq!(printed[0]["revision"].as_str(), Some("04948dcd8aa4"));
    let provenance = printed[0]["provenance"]
        .as_table()
        .expect("a provenance table");
    let expected = [
        ("registry_url", "http://127.0.0.1:9".to_string()),
        ("ref", "unknown/unknown:0.0.1".to_string()),
        ("manifest_digest", digest),
        ("source_digest", sha256_digest(source)),
        ("source_revision", "6bcfa90f".to_string()),
    ];
    assert_eq!(provenance.len(), expected.len(), "{provenance}");
    for (key, value) in expected {
        assert_eq!(provenance[key].as_str(), Some(value.as_str()), "{key}");
    }
}

const SENDER: &str = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef";
const RECEIVER: &str = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak";

fn transfer_args() -> serde_json::Value {
    serde_json::json!({
        "quantity": 3_000_000,
        "sender": SENDER,
        "receiver": RECEIVER,
        "middleman": SENDER
    })
}

/// Runs `fluent prepare` for `transfer_preprod` with the given arguments text.
fn prepare(config: &Path, registration: &str, args: &str) -> (Output, serde_json::Value) {
    let output = fluent(
        &[
            "prepare",
            "--config",
            config.to_str().expect("a UTF-8 path"),
            "--registration",
            registration,
            "--tx",
            "transfer",
            "--args",
            args,
        ],
        &[],
    );
    let printed = serde_json::from_str(&stdout(&output))
        .unwrap_or_else(|err| panic!("{err}: {}{}", stdout(&output), stderr(&output)));
    (output, printed)
}

#[tokio::test(flavor = "multi_thread")]
async fn prepare_prints_the_envelope() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let tx_hex = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../fluent-core/tests/fixtures/tx/transfer-preprod.hex"),
    )
    .unwrap();
    let hash = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": { "hash": hash, "tx": tx_hex.trim() }
        })))
        .mount(&server)
        .await;

    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prepare-envelope.toml");
    let text = format!(
        "[registrations]\ndir = {}\n\n[networks.preprod]\ntrp_url = \"{}\"\n\n\
         [auth]\nmode = \"none\"\n",
        toml::Value::String(registrations("valid").display().to_string()),
        server.uri()
    );
    std::fs::write(&path, text).unwrap();

    let args = transfer_args().to_string();
    let (output, envelope) =
        tokio::task::spawn_blocking(move || prepare(&path, "transfer_preprod", &args))
            .await
            .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(envelope["status"], "prepared_unsigned");
    assert_eq!(envelope["signed"], false);
    assert_eq!(envelope["submitted"], false);
    assert_eq!(envelope["tx_hash"], hash);
    assert_eq!(
        envelope["protocol"]["registration_slug"],
        "transfer_preprod"
    );
    assert_eq!(envelope["summary"]["outputs"][0]["address"], RECEIVER);
    assert_eq!(envelope["summary"]["outputs"][0]["lovelace"], 3_000_000);
}

#[test]
fn prepare_prints_errors_as_json_and_fails() {
    // The resolver is never contacted: every request fails before it.
    let config = config_for("prepare-errors", &registrations("valid"));

    let mut args = transfer_args();
    args["tax"] = serde_json::json!(1);
    let (output, printed) = prepare(&config, "transfer_preprod", &args.to_string());
    assert!(!output.status.success());
    assert_eq!(printed["error"]["code"], "invalid_arguments");
    assert_eq!(
        printed["error"]["message"],
        "invalid arguments: deployment-bound value cannot be overridden"
    );
    assert_eq!(
        printed["error"]["details"]["violations"],
        serde_json::json!([{ "path": "/tax", "message": "deployment-bound value cannot be overridden" }])
    );

    let (output, printed) = prepare(&config, "nothing_here", &transfer_args().to_string());
    assert!(!output.status.success());
    assert_eq!(printed["error"]["code"], "unknown_protocol");

    let (output, printed) = prepare(&config, "transfer_preprod", "{\"quantity\": 12345");
    assert!(!output.status.success());
    assert_eq!(printed["error"]["code"], "invalid_arguments");
    let message = printed["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with("invalid arguments: --args is not valid JSON"),
        "{message}"
    );
    assert!(!message.contains("12345"), "{message}");
}
