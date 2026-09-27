//! Server configuration.
//!
//! A [`Config`] is loaded from one TOML file and then adjusted by environment
//! overrides. Unknown keys, in the file or in an override, are errors.
//!
//! # Environment overrides
//!
//! A variable named `FLUENT_<TABLE>__<KEY>` overrides `<key>` in `[<table>]`,
//! and `FLUENT_NETWORKS__<NAME>__<KEY>` overrides `<key>` in
//! `[networks.<name>]`. Segments are separated by a double underscore and
//! lowercased, so `FLUENT_LIMITS__RESOLVER_TIMEOUT_SECS=10` sets
//! `limits.resolver_timeout_secs`. Overrides take precedence over the file and
//! may supply keys the file omits. Only `FLUENT_` variables that contain `__`
//! are overrides; others, such as a secret named by a `*_env` key, are ignored.
//!
//! # Secrets
//!
//! Secret-bearing keys end in `_env` and hold the *name* of an environment
//! variable. Its value is read once at load time into a [`SecretRef`], is never
//! serialized, and renders as `"<set>"` or `"<unset>"` in
//! [`Config::redacted`].

use std::collections::BTreeMap;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Prefix shared by every environment override.
pub const ENV_PREFIX: &str = "FLUENT_";

/// The complete server configuration.
///
/// Serializing a `Config` always produces the redacted form; see
/// [`Config::redacted`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Listener and public address.
    #[serde(default)]
    pub server: ServerConfig,
    /// Where protocol registrations are read from.
    pub registrations: RegistrationsConfig,
    /// TRP resolvers by network name, for example `mainnet` or `preprod`.
    pub networks: BTreeMap<String, NetworkConfig>,
    /// Timeouts, concurrency and quotas.
    #[serde(default)]
    pub limits: LimitsConfig,
    /// How callers authenticate.
    pub auth: AuthConfig,
    /// Persistent storage, when enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<StoreConfig>,
    /// The browser-facing site.
    #[serde(default)]
    pub site: SiteConfig,
}

/// `[server]`: listener and public address.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Socket address to listen on. Defaults to `127.0.0.1:8080`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// The externally visible base URL, when it differs from `listen`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            public_url: None,
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 8080))
}

/// `[registrations]`: where protocol registrations are read from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationsConfig {
    /// Directory holding registration files.
    pub dir: PathBuf,
}

/// `[networks.<name>]`: the TRP resolver for one network.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// TRP endpoint URL. Must not embed credentials.
    pub trp_url: String,
    /// Variable holding the TRP API key, for endpoints that require one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trp_api_key_env: Option<SecretRef>,
}

/// `[limits]`: timeouts, concurrency and quotas. Every key is optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    /// Seconds one resolver call may take. Defaults to 30.
    pub resolver_timeout_secs: u64,
    /// Seconds one request may take end to end. Defaults to 45; must not be
    /// shorter than `resolver_timeout_secs`.
    pub global_cutoff_secs: u64,
    /// Resolver calls in flight at once, server-wide. Defaults to 8.
    pub max_concurrent_resolutions: u32,
    /// Prepared transactions one caller may request per day. Defaults to 200.
    pub per_user_daily_quota: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            resolver_timeout_secs: 30,
            global_cutoff_secs: 45,
            max_concurrent_resolutions: 8,
            per_user_daily_quota: 200,
        }
    }
}

/// `[auth]`: how callers authenticate, selected by `mode`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum AuthConfig {
    /// `mode = "none"`: every caller is accepted.
    None,
    /// `mode = "token"`: callers present one static bearer token.
    Token {
        /// Variable holding the token.
        token_env: SecretRef,
    },
    /// `mode = "oidc"`: callers present a JWT from an OpenID Connect issuer.
    Oidc {
        /// Expected `iss` claim.
        issuer: String,
        /// URL of the issuer's JSON Web Key Set.
        jwks_url: String,
        /// Expected `aud` claim.
        audience: String,
    },
}

/// `[store]`: persistent storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// Path of the SQLite database file.
    pub sqlite_path: PathBuf,
}

/// `[site]`: the browser-facing site. Disabled by default; when enabled, every
/// other key is required.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    /// Whether the site is served.
    #[serde(default)]
    pub enabled: bool,
    /// Variable holding the key that signs session cookies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_secret_env: Option<SecretRef>,
    /// Variable holding the OIDC client ID used for sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_client_id_env: Option<SecretRef>,
    /// Variable holding the OIDC client secret used for sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc_client_secret_env: Option<SecretRef>,
    /// OIDC redirect URL registered with the identity provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_url: Option<String>,
}

/// A secret named by an environment variable and read at load time.
///
/// Deserializes from the variable name. Serializes and debug-prints only
/// whether a value was found, never the value.
#[derive(Clone)]
pub struct SecretRef {
    var: String,
    value: Option<String>,
}

impl SecretRef {
    /// The name of the environment variable.
    pub fn var(&self) -> &str {
        &self.var
    }

    /// The secret, if the variable was set to a non-empty value at load time.
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    /// Whether a value was found at load time.
    pub fn is_set(&self) -> bool {
        self.value.is_some()
    }

    fn status(&self) -> &'static str {
        if self.is_set() { "<set>" } else { "<unset>" }
    }

    fn resolve(&mut self, env: &BTreeMap<String, String>) {
        self.value = env.get(&self.var).filter(|v| !v.is_empty()).cloned();
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretRef")
            .field("var", &self.var)
            .field("value", &self.status())
            .finish()
    }
}

impl Serialize for SecretRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.status())
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(SecretRef {
            var: String::deserialize(deserializer)?,
            value: None,
        })
    }
}

/// Why a configuration could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file that was requested.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The TOML is malformed, has unknown keys or lacks required keys.
    #[error(transparent)]
    Parse(#[from] toml::de::Error),
    /// An environment override names an unknown key or has an invalid value.
    #[error("environment override {var}: {reason}")]
    Override {
        /// The offending variable.
        var: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The configuration parsed but breaks one or more rules.
    #[error("invalid configuration: {}", .problems.join("; "))]
    Invalid {
        /// Every rule that failed.
        problems: Vec<String>,
    },
}

impl Config {
    /// Loads `path` with overrides and secrets from the process environment.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::load_with_env(path, process_env())
    }

    /// Loads `path` with overrides and secrets from `env` instead of the
    /// process environment.
    pub fn load_with_env<I, K, V>(path: impl AsRef<Path>, env: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_toml_str(&text, env)
    }

    /// Parses TOML `text`, applies overrides and reads secrets from `env`, then
    /// validates the result.
    pub fn from_toml_str<I, K, V>(text: &str, env: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let env: BTreeMap<String, String> =
            env.into_iter().map(|(k, v)| (k.into(), v.into())).collect();

        let mut table: toml::Table = toml::from_str(text)?;
        let mut config: Config = if apply_overrides(&mut table, &env)? {
            toml::Value::Table(table).try_into()?
        } else {
            // Parsing the text again keeps line and column in error messages.
            toml::from_str(text)?
        };

        for secret in config.secrets_mut() {
            secret.resolve(&env);
        }
        config.validate()?;
        Ok(config)
    }

    /// Checks the rules the types alone cannot express.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut problems = Vec::new();

        if let Some(url) = &self.server.public_url {
            check_url(&mut problems, "server.public_url", url);
        }
        if self.registrations.dir.as_os_str().is_empty() {
            problems.push("registrations.dir must not be empty".to_string());
        }

        if self.networks.is_empty() {
            problems.push("at least one [networks.<name>] table is required".to_string());
        }
        for (name, network) in &self.networks {
            if !is_network_name(name) {
                problems.push(format!(
                    "network name `{name}` must use lowercase letters, digits, `_` or `-`"
                ));
            }
            check_url(
                &mut problems,
                &format!("networks.{name}.trp_url"),
                &network.trp_url,
            );
        }

        let limits = &self.limits;
        for (key, value) in [
            ("resolver_timeout_secs", limits.resolver_timeout_secs),
            ("global_cutoff_secs", limits.global_cutoff_secs),
            (
                "max_concurrent_resolutions",
                limits.max_concurrent_resolutions.into(),
            ),
            ("per_user_daily_quota", limits.per_user_daily_quota.into()),
        ] {
            if value == 0 {
                problems.push(format!("limits.{key} must be greater than zero"));
            }
        }
        if limits.resolver_timeout_secs > limits.global_cutoff_secs {
            problems.push(
                "limits.resolver_timeout_secs must not exceed limits.global_cutoff_secs"
                    .to_string(),
            );
        }

        if let AuthConfig::Oidc {
            issuer,
            jwks_url,
            audience,
        } = &self.auth
        {
            check_url(&mut problems, "auth.issuer", issuer);
            check_url(&mut problems, "auth.jwks_url", jwks_url);
            if audience.is_empty() {
                problems.push("auth.audience must not be empty".to_string());
            }
        }

        if let Some(store) = &self.store
            && store.sqlite_path.as_os_str().is_empty()
        {
            problems.push("store.sqlite_path must not be empty".to_string());
        }

        let site = &self.site;
        if site.enabled {
            for (key, present) in [
                ("session_secret_env", site.session_secret_env.is_some()),
                ("oidc_client_id_env", site.oidc_client_id_env.is_some()),
                (
                    "oidc_client_secret_env",
                    site.oidc_client_secret_env.is_some(),
                ),
                ("redirect_url", site.redirect_url.is_some()),
            ] {
                if !present {
                    problems.push(format!("site.{key} is required when site.enabled = true"));
                }
            }
        }
        if let Some(url) = &site.redirect_url {
            check_url(&mut problems, "site.redirect_url", url);
        }

        for secret in self.secrets() {
            if !is_env_var_name(secret.var()) {
                problems.push(format!(
                    "`{}` is not a valid environment variable name",
                    secret.var()
                ));
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid { problems })
        }
    }

    /// Renders the configuration as TOML for logs, with every secret-bearing
    /// key replaced by `"<set>"` or `"<unset>"`.
    pub fn redacted(&self) -> String {
        toml::to_string(self)
            .unwrap_or_else(|err| format!("# configuration could not be rendered: {err}\n"))
    }

    fn secrets(&self) -> impl Iterator<Item = &SecretRef> {
        let networks = self
            .networks
            .values()
            .filter_map(|n| n.trp_api_key_env.as_ref());
        let auth = match &self.auth {
            AuthConfig::Token { token_env } => Some(token_env),
            AuthConfig::None | AuthConfig::Oidc { .. } => None,
        };
        let site = [
            &self.site.session_secret_env,
            &self.site.oidc_client_id_env,
            &self.site.oidc_client_secret_env,
        ]
        .into_iter()
        .flatten();
        networks.chain(auth).chain(site)
    }

    fn secrets_mut(&mut self) -> impl Iterator<Item = &mut SecretRef> {
        let networks = self
            .networks
            .values_mut()
            .filter_map(|n| n.trp_api_key_env.as_mut());
        let auth = match &mut self.auth {
            AuthConfig::Token { token_env } => Some(token_env),
            AuthConfig::None | AuthConfig::Oidc { .. } => None,
        };
        let site = [
            &mut self.site.session_secret_env,
            &mut self.site.oidc_client_id_env,
            &mut self.site.oidc_client_secret_env,
        ]
        .into_iter()
        .flatten();
        networks.chain(auth).chain(site)
    }
}

fn process_env() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
}

#[derive(Clone, Copy)]
enum ValueKind {
    String,
    Integer,
    Boolean,
}

/// The type of the key an override path names, or `None` for unknown keys.
fn override_kind(path: &[&str]) -> Option<ValueKind> {
    let kind = match path {
        ["server", "listen" | "public_url"] => ValueKind::String,
        ["registrations", "dir"] => ValueKind::String,
        ["networks", name, "trp_url" | "trp_api_key_env"] if !name.is_empty() => ValueKind::String,
        [
            "limits",
            "resolver_timeout_secs"
            | "global_cutoff_secs"
            | "max_concurrent_resolutions"
            | "per_user_daily_quota",
        ] => ValueKind::Integer,
        [
            "auth",
            "mode" | "token_env" | "issuer" | "jwks_url" | "audience",
        ] => ValueKind::String,
        ["store", "sqlite_path"] => ValueKind::String,
        ["site", "enabled"] => ValueKind::Boolean,
        [
            "site",
            "session_secret_env" | "oidc_client_id_env" | "oidc_client_secret_env" | "redirect_url",
        ] => ValueKind::String,
        _ => return None,
    };
    Some(kind)
}

/// Applies every `FLUENT_*__*` variable to `table`. Returns whether any applied.
fn apply_overrides(
    table: &mut toml::Table,
    env: &BTreeMap<String, String>,
) -> Result<bool, ConfigError> {
    let mut applied = false;
    for (var, raw) in env {
        let Some(rest) = var.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        if !rest.contains("__") {
            continue;
        }
        let fail = |reason: &str| ConfigError::Override {
            var: var.clone(),
            reason: reason.to_string(),
        };

        let path: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
        let segments: Vec<&str> = path.iter().map(String::as_str).collect();
        let kind = override_kind(&segments).ok_or_else(|| fail("unknown configuration key"))?;

        let value = match kind {
            ValueKind::String => toml::Value::String(raw.clone()),
            ValueKind::Integer => raw
                .trim()
                .parse::<u64>()
                .ok()
                .and_then(|n| i64::try_from(n).ok())
                .map(toml::Value::Integer)
                .ok_or_else(|| fail("expected a non-negative integer"))?,
            ValueKind::Boolean => match raw.trim().to_ascii_lowercase().as_str() {
                "true" => toml::Value::Boolean(true),
                "false" => toml::Value::Boolean(false),
                _ => return Err(fail("expected `true` or `false`")),
            },
        };

        let (key, parents) = segments.split_last().ok_or_else(|| fail("empty key"))?;
        let mut current = &mut *table;
        for parent in parents {
            current = current
                .entry(parent.to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| fail("the file defines this parent key as a value, not a table"))?;
        }
        current.insert(key.to_string(), value);
        applied = true;
    }
    Ok(applied)
}

fn check_url(problems: &mut Vec<String>, key: &str, url: &str) {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        problems.push(format!("{key} must be an http:// or https:// URL"));
        return;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || url.chars().any(char::is_whitespace) {
        problems.push(format!("{key} must be an http:// or https:// URL"));
    } else if authority.contains('@') {
        problems.push(format!(
            "{key} must not embed credentials; name them with a *_env key"
        ));
    }
}

fn is_network_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn is_env_var_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [registrations]
        dir = "registrations"

        [networks.preprod]
        trp_url = "https://cardano-preprod.trp-m1.demeter.run"
        trp_api_key_env = "TRP_PREPROD_API_KEY"

        [auth]
        mode = "token"
        token_env = "FLUENT_API_TOKEN"
    "#;

    const NO_ENV: [(&str, &str); 0] = [];

    fn problems(result: Result<Config, ConfigError>) -> Vec<String> {
        match result {
            Err(ConfigError::Invalid { problems }) => problems,
            other => panic!("expected validation problems, got {other:?}"),
        }
    }

    #[test]
    fn applies_defaults() {
        let config = Config::from_toml_str(MINIMAL, NO_ENV).unwrap();
        assert_eq!(config.server.listen.to_string(), "127.0.0.1:8080");
        assert_eq!(config.server.public_url, None);
        assert_eq!(config.limits, LimitsConfig::default());
        assert_eq!(config.limits.resolver_timeout_secs, 30);
        assert_eq!(config.limits.global_cutoff_secs, 45);
        assert_eq!(config.limits.max_concurrent_resolutions, 8);
        assert_eq!(config.limits.per_user_daily_quota, 200);
        assert!(config.store.is_none());
        assert!(!config.site.enabled);
    }

    #[test]
    fn rejects_unknown_keys_in_every_table() {
        for extra in [
            "surprise = 1",
            "[server]\nlistn = \"127.0.0.1:1\"",
            "[limits]\nretries = 3",
            "[store]\nsqlite_path = \"x\"\npassword = \"p\"",
            "[site]\ntheme = \"dark\"",
            "[networks.preview]\ntrp_url = \"https://x\"\napi_key = \"k\"",
        ] {
            let text = format!("{extra}\n{MINIMAL}");
            let err = Config::from_toml_str(&text, NO_ENV).unwrap_err();
            assert!(
                matches!(err, ConfigError::Parse(_)),
                "accepted `{extra}`: {err}"
            );
            assert!(err.to_string().contains("unknown field"), "{err}");
        }
    }

    #[test]
    fn rejects_keys_from_another_auth_mode() {
        let text = MINIMAL.replace(
            "token_env = \"FLUENT_API_TOKEN\"",
            "token_env = \"FLUENT_API_TOKEN\"\nissuer = \"https://id.example\"",
        );
        let err = Config::from_toml_str(&text, NO_ENV).unwrap_err();
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn requires_the_keys_of_the_selected_auth_mode() {
        let text = MINIMAL.replace("token_env = \"FLUENT_API_TOKEN\"", "");
        let err = Config::from_toml_str(&text, NO_ENV).unwrap_err();
        assert!(err.to_string().contains("token_env"), "{err}");

        let text = MINIMAL.replace(
            "mode = \"token\"\n        token_env = \"FLUENT_API_TOKEN\"",
            "mode = \"oidc\"\nissuer = \"https://id.example\"",
        );
        let err = Config::from_toml_str(&text, NO_ENV).unwrap_err();
        assert!(err.to_string().contains("jwks_url"), "{err}");
    }

    #[test]
    fn reads_secrets_without_exposing_them() {
        let config = Config::from_toml_str(
            MINIMAL,
            [
                ("FLUENT_API_TOKEN", "tok-7d1c9e"),
                ("TRP_PREPROD_API_KEY", ""),
            ],
        )
        .unwrap();
        let AuthConfig::Token { token_env } = &config.auth else {
            panic!("expected token auth");
        };
        assert_eq!(token_env.var(), "FLUENT_API_TOKEN");
        assert_eq!(token_env.value(), Some("tok-7d1c9e"));
        let preprod = config.networks["preprod"].trp_api_key_env.as_ref().unwrap();
        assert!(!preprod.is_set(), "empty values count as unset");

        let debug = format!("{config:?}");
        assert!(!debug.contains("tok-7d1c9e"), "{debug}");
        assert!(debug.contains("<set>"), "{debug}");
    }

    #[test]
    fn redacted_replaces_secret_keys_with_their_status() {
        let config = Config::from_toml_str(MINIMAL, [("FLUENT_API_TOKEN", "tok-7d1c9e")]).unwrap();
        let redacted = config.redacted();
        assert!(redacted.contains("token_env = \"<set>\""), "{redacted}");
        assert!(
            redacted.contains("trp_api_key_env = \"<unset>\""),
            "{redacted}"
        );
        assert!(!redacted.contains("tok-7d1c9e"), "{redacted}");
        assert!(!redacted.contains("FLUENT_API_TOKEN"), "{redacted}");
    }

    #[test]
    fn overrides_take_precedence_and_can_add_keys() {
        let config = Config::from_toml_str(
            MINIMAL,
            [
                ("FLUENT_SERVER__LISTEN", "0.0.0.0:9000"),
                ("FLUENT_LIMITS__RESOLVER_TIMEOUT_SECS", "10"),
                ("FLUENT_NETWORKS__PREPROD__TRP_URL", "https://trp.example"),
                ("FLUENT_NETWORKS__DEVNET__TRP_URL", "http://localhost:8164"),
                ("FLUENT_STORE__SQLITE_PATH", "/data/fluent.sqlite"),
                ("FLUENT_SITE__ENABLED", "false"),
            ],
        )
        .unwrap();
        assert_eq!(config.server.listen.to_string(), "0.0.0.0:9000");
        assert_eq!(config.limits.resolver_timeout_secs, 10);
        assert_eq!(config.limits.global_cutoff_secs, 45);
        assert_eq!(config.networks["preprod"].trp_url, "https://trp.example");
        assert_eq!(config.networks["devnet"].trp_url, "http://localhost:8164");
        assert_eq!(
            config.store.unwrap().sqlite_path,
            PathBuf::from("/data/fluent.sqlite")
        );
    }

    #[test]
    fn override_errors_name_the_variable_but_not_its_value() {
        for (var, value, reason) in [
            ("FLUENT_SERVER__LISTN", "x", "unknown configuration key"),
            ("FLUENT_SERVR__LISTEN", "x", "unknown configuration key"),
            (
                "FLUENT_NETWORKS____TRP_URL",
                "x",
                "unknown configuration key",
            ),
            ("FLUENT_LIMITS__GLOBAL_CUTOFF_SECS", "s3cr3t", "integer"),
            ("FLUENT_SITE__ENABLED", "s3cr3t", "true"),
        ] {
            let err = Config::from_toml_str(MINIMAL, [(var, value)]).unwrap_err();
            let text = err.to_string();
            assert!(matches!(err, ConfigError::Override { .. }), "{text}");
            assert!(text.contains(var) && text.contains(reason), "{text}");
            assert!(!text.contains("s3cr3t"), "{text}");
        }
    }

    #[test]
    fn ignores_fluent_variables_that_are_not_overrides() {
        Config::from_toml_str(
            MINIMAL,
            [("FLUENT_API_TOKEN", "t"), ("FLUENT_SOMETHING", "x")],
        )
        .unwrap();
    }

    #[test]
    fn validates_limits() {
        let found = problems(Config::from_toml_str(
            MINIMAL,
            [
                ("FLUENT_LIMITS__RESOLVER_TIMEOUT_SECS", "60"),
                ("FLUENT_LIMITS__MAX_CONCURRENT_RESOLUTIONS", "0"),
            ],
        ));
        assert_eq!(
            found,
            [
                "limits.max_concurrent_resolutions must be greater than zero",
                "limits.resolver_timeout_secs must not exceed limits.global_cutoff_secs",
            ]
        );
    }

    #[test]
    fn validates_urls_and_names() {
        let text = format!(
            "{MINIMAL}\n[networks.Bad]\ntrp_url = \"https://user:key@trp.example\"\n\
             trp_api_key_env = \"not-a-var\"\n[server]\npublic_url = \"ftp://x\""
        );
        let found = problems(Config::from_toml_str(&text, NO_ENV));
        assert_eq!(
            found,
            [
                "server.public_url must be an http:// or https:// URL",
                "network name `Bad` must use lowercase letters, digits, `_` or `-`",
                "networks.Bad.trp_url must not embed credentials; name them with a *_env key",
                "`not-a-var` is not a valid environment variable name",
            ]
        );
    }

    #[test]
    fn requires_a_network() {
        let text = MINIMAL.replace(
            "[networks.preprod]\n        trp_url = \"https://cardano-preprod.trp-m1.demeter.run\"\n        trp_api_key_env = \"TRP_PREPROD_API_KEY\"",
            "[networks]",
        );
        let found = problems(Config::from_toml_str(&text, NO_ENV));
        assert_eq!(found, ["at least one [networks.<name>] table is required"]);
    }

    #[test]
    fn enabled_site_requires_its_keys() {
        let found = problems(Config::from_toml_str(
            MINIMAL,
            [("FLUENT_SITE__ENABLED", "true")],
        ));
        assert_eq!(
            found,
            [
                "site.session_secret_env is required when site.enabled = true",
                "site.oidc_client_id_env is required when site.enabled = true",
                "site.oidc_client_secret_env is required when site.enabled = true",
                "site.redirect_url is required when site.enabled = true",
            ]
        );
    }

    #[test]
    fn syntax_errors_report_their_location() {
        let err = Config::from_toml_str("[server\nlisten = 1", NO_ENV).unwrap_err();
        assert!(err.to_string().contains("line 1"), "{err}");
    }
}
