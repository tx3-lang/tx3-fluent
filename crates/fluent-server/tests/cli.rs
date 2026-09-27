//! End-to-end checks of the `fluent` binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
