//! End-to-end checks of the `fluent` binary.

use std::path::PathBuf;
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
