//! The example configurations shipped in `examples/config/` must stay valid.

use std::path::PathBuf;

use fluent_core::config::{AuthConfig, Config};

const TABLES: [&str; 7] = [
    "server",
    "registrations",
    "networks",
    "limits",
    "auth",
    "store",
    "site",
];

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/config")
        .join(name)
}

fn load(name: &str, env: &[(&str, &str)]) -> Config {
    Config::load_with_env(example(name), env.iter().copied())
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

#[test]
fn every_example_exercises_every_table() {
    for name in ["self-hosted.toml", "hosted.toml"] {
        let text = std::fs::read_to_string(example(name)).unwrap();
        let table: toml::Table = toml::from_str(&text).unwrap();
        for key in TABLES {
            assert!(table.contains_key(key), "{name} lacks [{key}]");
        }
    }
}

#[test]
fn self_hosted_example_loads() {
    let config = load("self-hosted.toml", &[("FLUENT_API_TOKEN", "t")]);
    assert!(matches!(config.auth, AuthConfig::Token { .. }));
    assert_eq!(
        config.networks.keys().collect::<Vec<_>>(),
        ["devnet", "preprod"]
    );
    assert!(!config.site.enabled);
    assert!(config.store.is_some());
}

#[test]
fn hosted_example_loads() {
    let config = load("hosted.toml", &[]);
    assert!(matches!(config.auth, AuthConfig::Oidc { .. }));
    assert_eq!(
        config.networks.keys().collect::<Vec<_>>(),
        ["mainnet", "preprod"]
    );
    assert!(config.site.enabled);
    assert_eq!(config.server.listen.to_string(), "0.0.0.0:8080");
}

#[test]
fn redacted_never_emits_a_value_read_from_an_env_variable() {
    // One distinct sentinel per secret-bearing key across both examples.
    let secrets = [
        ("TRP_MAINNET_API_KEY", "sentinel-mainnet-6f2a"),
        ("TRP_PREPROD_API_KEY", "sentinel-preprod-91bd"),
        ("FLUENT_API_TOKEN", "sentinel-token-33c8"),
        ("FLUENT_SESSION_SECRET", "sentinel-session-0c4e"),
        ("FLUENT_OIDC_CLIENT_ID", "sentinel-client-id-7a13"),
        ("FLUENT_OIDC_CLIENT_SECRET", "sentinel-client-secret-e58d"),
    ];
    for name in ["self-hosted.toml", "hosted.toml"] {
        let config = load(name, &secrets);
        for rendered in [config.redacted(), format!("{config:?}")] {
            for (var, value) in secrets {
                assert!(!rendered.contains(value), "{name} leaked {var}: {rendered}");
            }
        }
        let redacted = config.redacted();
        for (var, _) in secrets {
            assert!(!redacted.contains(var), "{name} named {var}: {redacted}");
        }
        assert!(redacted.contains("\"<set>\""), "{redacted}");
        assert!(!redacted.contains("\"<unset>\""), "{redacted}");
    }

    let unset = load("hosted.toml", &[]).redacted();
    assert!(unset.contains("\"<unset>\""), "{unset}");
    assert!(!unset.contains("\"<set>\""), "{unset}");
}
